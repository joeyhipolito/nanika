package api

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"mime/multipart"
	"net/http"
	"os"
	"path/filepath"
	"strings"
)

// BuildWords converts character-level alignment data into word-level timing slices.
// Words are delimited by whitespace. Returns an empty (non-nil) slice when alignment is empty.
func BuildWords(alignment *Alignment) []Word {
	if alignment == nil || len(alignment.Characters) == 0 {
		return []Word{}
	}

	var words []Word
	var currentWord strings.Builder
	var wordStart float64
	prevWasSpace := false

	for i, char := range alignment.Characters {
		isSpace := char == " " || char == "\n" || char == "\t"

		if isSpace && currentWord.Len() > 0 && !prevWasSpace {
			words = append(words, Word{
				Word:  currentWord.String(),
				Start: wordStart,
				End:   alignment.CharacterEndTimes[i-1],
			})
			currentWord.Reset()
		}

		if isSpace {
			prevWasSpace = true
			continue
		}

		if currentWord.Len() == 0 {
			wordStart = alignment.CharacterStartTimes[i]
		}
		currentWord.WriteString(char)
		prevWasSpace = false
	}

	if currentWord.Len() > 0 {
		words = append(words, Word{
			Word:  currentWord.String(),
			Start: wordStart,
			End:   alignment.CharacterEndTimes[len(alignment.CharacterEndTimes)-1],
		})
	}

	return words
}

// ForcedAlignment calls POST /v1/forced-alignment with multipart/form-data
// containing the audio file binary and clean transcript text.
// Returns character-level alignment data.
func (c *Client) ForcedAlignment(ctx context.Context, audioPath, text string) (*Alignment, error) {
	if audioPath == "" {
		return nil, fmt.Errorf("audio path is required")
	}
	if text == "" {
		return nil, fmt.Errorf("text is required")
	}

	f, err := os.Open(audioPath)
	if err != nil {
		return nil, fmt.Errorf("opening audio file %s: %w", audioPath, err)
	}
	defer f.Close()

	var buf bytes.Buffer
	mw := multipart.NewWriter(&buf)

	fw, err := mw.CreateFormFile("file", filepath.Base(audioPath))
	if err != nil {
		return nil, fmt.Errorf("creating form file: %w", err)
	}
	if _, err := io.Copy(fw, f); err != nil {
		return nil, fmt.Errorf("copying audio data: %w", err)
	}
	if err := mw.WriteField("text", text); err != nil {
		return nil, fmt.Errorf("writing text field: %w", err)
	}
	if err := mw.Close(); err != nil {
		return nil, fmt.Errorf("finalizing multipart body: %w", err)
	}

	body := buf.Bytes()
	httpReq, err := c.newRequest(ctx, http.MethodPost, "/v1/forced-alignment")
	if err != nil {
		return nil, err
	}
	httpReq.Header.Set("Content-Type", mw.FormDataContentType())
	httpReq.Body = nopCloser{bytes.NewReader(body)}
	httpReq.ContentLength = int64(len(body))

	resp, err := c.httpClient.Do(httpReq)
	if err != nil {
		return nil, fmt.Errorf("POST /v1/forced-alignment: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("forced-alignment API returned status %d", resp.StatusCode)
	}

	var al Alignment
	if err := json.NewDecoder(resp.Body).Decode(&al); err != nil {
		return nil, fmt.Errorf("decoding forced-alignment response: %w", err)
	}
	return &al, nil
}
