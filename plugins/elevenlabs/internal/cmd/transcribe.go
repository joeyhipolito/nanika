package cmd

import (
	"bytes"
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"mime/multipart"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-elevenlabs/internal/config"
)

const (
	transcribeDefaultTimeout = 120 * time.Second
	scribeURL                = "https://api.elevenlabs.io/v1/speech-to-text"
)

// TranscribeCmd transcribes an audio file to text using whisper or ElevenLabs Scribe.
func TranscribeCmd(args []string) error {
	fs := flag.NewFlagSet("transcribe", flag.ContinueOnError)
	var (
		input    string
		provider string
		timeout  time.Duration
	)
	fs.StringVar(&input, "input", "", "path to audio file (.ogg, .mp3, .wav, .webm)")
	fs.StringVar(&provider, "provider", "scribe", "transcription provider: whisper or scribe")
	fs.DurationVar(&timeout, "timeout", transcribeDefaultTimeout, "max transcription time")
	if err := fs.Parse(args); err != nil {
		return err
	}

	if input == "" {
		return fmt.Errorf("--input is required")
	}
	if _, err := os.Stat(input); err != nil {
		return fmt.Errorf("input file not found: %s", input)
	}

	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()

	var text string
	var err error

	switch provider {
	case "whisper":
		text, err = transcribeWhisper(ctx, input)
	case "scribe":
		apiKey, keyErr := resolveAPIKey()
		if keyErr != nil {
			return keyErr
		}
		text, err = transcribeScribe(ctx, input, apiKey)
	default:
		return fmt.Errorf("unknown provider %q (use whisper or scribe)", provider)
	}

	if err != nil {
		return err
	}

	fmt.Print(strings.TrimSpace(text))
	return nil
}

// resolveAPIKey returns the ElevenLabs API key from config file or env var.
func resolveAPIKey() (string, error) {
	cfg, err := config.Load()
	if err == nil && cfg.APIKey != "" {
		return cfg.APIKey, nil
	}
	if key := os.Getenv("ELEVENLABS_API_KEY"); key != "" {
		return key, nil
	}
	return "", fmt.Errorf("ElevenLabs API key not found: run 'elevenlabs configure' or set ELEVENLABS_API_KEY")
}

// transcribeWhisper shells out to mlx_whisper or whisper CLI.
func transcribeWhisper(ctx context.Context, inputPath string) (string, error) {
	whisperBin := ""
	for _, candidate := range []string{"mlx_whisper", "whisper"} {
		if path, err := exec.LookPath(candidate); err == nil {
			whisperBin = path
			break
		}
	}
	if whisperBin == "" {
		return "", fmt.Errorf("whisper not found — install with: pip install openai-whisper (or pip install mlx-whisper for Apple Silicon)")
	}

	tmpDir, err := os.MkdirTemp("", "elevenlabs-transcribe-*")
	if err != nil {
		return "", fmt.Errorf("creating temp dir: %w", err)
	}
	defer os.RemoveAll(tmpDir)

	args := []string{
		inputPath,
		"--output_dir", tmpDir,
		"--output_format", "txt",
		"--language", "en",
	}

	cmd := exec.CommandContext(ctx, whisperBin, args...)
	cmd.Stderr = os.Stderr
	if err := cmd.Run(); err != nil {
		return "", fmt.Errorf("whisper failed: %w", err)
	}

	stem := strings.TrimSuffix(filepath.Base(inputPath), filepath.Ext(inputPath))
	outputPath := filepath.Join(tmpDir, stem+".txt")

	// Fallback: whisper sometimes ignores --output_dir, check input dir too.
	if _, err := os.Stat(outputPath); err != nil {
		altPath := filepath.Join(filepath.Dir(inputPath), stem+".txt")
		if _, err2 := os.Stat(altPath); err2 == nil {
			outputPath = altPath
			defer os.Remove(altPath)
		} else {
			return "", fmt.Errorf("whisper output not found at %s or %s", outputPath, altPath)
		}
	}

	data, err := os.ReadFile(outputPath)
	if err != nil {
		return "", fmt.Errorf("reading whisper output: %w", err)
	}
	return string(data), nil
}

// transcribeScribe sends the audio file to the ElevenLabs Scribe API.
func transcribeScribe(ctx context.Context, inputPath, apiKey string) (string, error) {
	audioData, err := os.ReadFile(inputPath)
	if err != nil {
		return "", fmt.Errorf("reading audio file: %w", err)
	}

	var body bytes.Buffer
	writer := multipart.NewWriter(&body)
	part, err := writer.CreateFormFile("file", filepath.Base(inputPath))
	if err != nil {
		return "", fmt.Errorf("creating form file: %w", err)
	}
	if _, err := part.Write(audioData); err != nil {
		return "", fmt.Errorf("writing audio data: %w", err)
	}
	if err := writer.WriteField("model_id", "scribe_v1"); err != nil {
		return "", fmt.Errorf("writing model_id field: %w", err)
	}
	writer.Close()

	req, err := http.NewRequestWithContext(ctx, "POST", scribeURL, &body)
	if err != nil {
		return "", fmt.Errorf("creating request: %w", err)
	}
	req.Header.Set("Content-Type", writer.FormDataContentType())
	req.Header.Set("xi-api-key", apiKey)

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return "", fmt.Errorf("Scribe API request failed: %w", err)
	}
	defer resp.Body.Close()

	respBody, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return "", fmt.Errorf("reading response: %w", err)
	}

	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("Scribe API error %d: %s", resp.StatusCode, string(respBody))
	}

	var result struct {
		Text string `json:"text"`
	}
	if err := json.Unmarshal(respBody, &result); err != nil {
		return "", fmt.Errorf("parsing response: %w", err)
	}

	return result.Text, nil
}
