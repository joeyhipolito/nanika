package api

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
)

// GenerateWithTimestamps calls POST /v1/text-to-speech/{voice_id}/with-timestamps
// and returns the audio bytes (decoded from base64) and alignment data.
// outputFormat is optional; if empty, the API uses its default.
func (c *Client) GenerateWithTimestamps(ctx context.Context, voiceID string, req TTSRequest, outputFormat string) ([]byte, *Alignment, error) {
	if voiceID == "" {
		return nil, nil, fmt.Errorf("voice_id is required")
	}

	body, err := json.Marshal(req)
	if err != nil {
		return nil, nil, fmt.Errorf("marshalling TTS request: %w", err)
	}

	path := "/v1/text-to-speech/" + voiceID + "/with-timestamps"
	if outputFormat != "" {
		q := url.Values{}
		q.Set("output_format", outputFormat)
		path += "?" + q.Encode()
	}

	httpReq, err := c.newRequest(ctx, http.MethodPost, path)
	if err != nil {
		return nil, nil, err
	}
	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Body = nopCloser{bytes.NewReader(body)}
	httpReq.ContentLength = int64(len(body))

	resp, err := c.httpClient.Do(httpReq)
	if err != nil {
		return nil, nil, fmt.Errorf("POST /v1/text-to-speech: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, nil, fmt.Errorf("TTS API returned status %d", resp.StatusCode)
	}

	var ttsResp TTSResponse
	if err := json.NewDecoder(resp.Body).Decode(&ttsResp); err != nil {
		return nil, nil, fmt.Errorf("decoding TTS response: %w", err)
	}

	audioBytes, err := decodeBase64Audio(ttsResp.AudioBase64)
	if err != nil {
		return nil, nil, fmt.Errorf("decoding audio base64: %w", err)
	}

	return audioBytes, ttsResp.Alignment, nil
}

// decodeBase64Audio decodes the base64-encoded audio field from the TTS response.
func decodeBase64Audio(encoded string) ([]byte, error) {
	if encoded == "" {
		return nil, fmt.Errorf("empty audio_base64 in response")
	}
	decoded, err := base64.StdEncoding.DecodeString(encoded)
	if err != nil {
		return nil, fmt.Errorf("base64 decode: %w", err)
	}
	return decoded, nil
}

// nopCloser wraps a bytes.Reader to satisfy io.ReadCloser.
type nopCloser struct{ *bytes.Reader }

func (nopCloser) Close() error { return nil }
