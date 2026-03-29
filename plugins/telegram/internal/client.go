// Package internal provides a minimal Telegram Bot API client for voice message operations.
package internal

import (
	"bytes"
	"context"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"math"
	"mime/multipart"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

// Client is a minimal Telegram Bot API client.
type Client struct {
	token   string
	http    *http.Client
	apiBase string
}

// Config holds the Telegram plugin configuration, read from ~/.alluka/channels/telegram.json.
type Config struct {
	BotToken string   `json:"bot_token"`
	ChatIDs  []string `json:"chat_ids"`
}

// NewClient creates a Telegram client authenticated with the given bot token.
func NewClient(token string) *Client {
	return &Client{
		token:   token,
		http:    &http.Client{Timeout: 60 * time.Second},
		apiBase: "https://api.telegram.org/bot" + token,
	}
}

// LoadConfig reads the Telegram config from ~/.alluka/channels/telegram.json.
func LoadConfig() (*Config, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil, fmt.Errorf("getting home dir: %w", err)
	}
	path := filepath.Join(home, ".alluka", "channels", "telegram.json")
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("reading %s: %w", path, err)
	}
	var cfg Config
	if err := json.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("parsing %s: %w", path, err)
	}
	if cfg.BotToken == "" {
		return nil, fmt.Errorf("%s: bot_token is required", path)
	}
	return &cfg, nil
}

// BotInfo holds the result of getMe.
type BotInfo struct {
	ID        int64  `json:"id"`
	IsBot     bool   `json:"is_bot"`
	FirstName string `json:"first_name"`
	Username  string `json:"username"`
}

// GetMe calls getMe and returns bot identity info.
func (c *Client) GetMe(ctx context.Context) (*BotInfo, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, c.apiBase+"/getMe", nil)
	if err != nil {
		return nil, fmt.Errorf("creating request: %w", err)
	}
	resp, err := c.http.Do(req)
	if err != nil {
		return nil, fmt.Errorf("getMe: %w", err)
	}
	defer resp.Body.Close()
	data, _ := io.ReadAll(io.LimitReader(resp.Body, 64*1024))

	var result struct {
		OK     bool    `json:"ok"`
		Result BotInfo `json:"result"`
	}
	if err := json.Unmarshal(data, &result); err != nil {
		return nil, fmt.Errorf("parsing getMe response: %w", err)
	}
	if !result.OK {
		return nil, fmt.Errorf("getMe returned ok=false: %s", string(data))
	}
	return &result.Result, nil
}

// SendMessage sends a plain text message to a Telegram chat.
func (c *Client) SendMessage(ctx context.Context, chatID, text string) error {
	payload, err := json.Marshal(map[string]any{
		"chat_id": chatID,
		"text":    text,
	})
	if err != nil {
		return fmt.Errorf("marshal: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, c.apiBase+"/sendMessage", bytes.NewReader(payload))
	if err != nil {
		return fmt.Errorf("creating request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := c.http.Do(req)
	if err != nil {
		return fmt.Errorf("sendMessage: %w", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(io.LimitReader(resp.Body, 64*1024))
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("sendMessage returned %d: %s", resp.StatusCode, string(body))
	}
	var result struct {
		OK bool `json:"ok"`
	}
	if err := json.Unmarshal(body, &result); err != nil {
		return fmt.Errorf("parsing sendMessage response: %w", err)
	}
	if !result.OK {
		return fmt.Errorf("sendMessage returned ok=false: %s", string(body))
	}
	return nil
}

// SendVoice sends an audio file as a Telegram voice message (OGG/Opus).
//
// Flow:
//  1. Convert to OGG/Opus (48kHz, 32kbps, mono) via ffmpeg
//  2. Generate 64-sample waveform + get duration
//  3. POST multipart/form-data to sendVoice with chat_id, voice file, and duration
func (c *Client) SendVoice(ctx context.Context, chatID, audioPath string) error {
	oggPath, err := ConvertToOgg(audioPath)
	if err != nil {
		return fmt.Errorf("converting audio: %w", err)
	}
	defer os.Remove(oggPath)

	// Generate waveform for logging / future use; Telegram doesn't render it natively.
	_, err = GenerateWaveform(oggPath)
	if err != nil {
		return fmt.Errorf("generating waveform: %w", err)
	}

	duration, err := audioDuration(oggPath)
	if err != nil {
		return fmt.Errorf("getting duration: %w", err)
	}

	f, err := os.Open(oggPath)
	if err != nil {
		return fmt.Errorf("opening ogg: %w", err)
	}
	defer f.Close()

	var buf bytes.Buffer
	mw := multipart.NewWriter(&buf)

	if err := mw.WriteField("chat_id", chatID); err != nil {
		return fmt.Errorf("writing chat_id field: %w", err)
	}
	if err := mw.WriteField("duration", strconv.Itoa(int(math.Round(duration)))); err != nil {
		return fmt.Errorf("writing duration field: %w", err)
	}

	fw, err := mw.CreateFormFile("voice", "voice-message.ogg")
	if err != nil {
		return fmt.Errorf("creating form file: %w", err)
	}
	if _, err := io.Copy(fw, f); err != nil {
		return fmt.Errorf("copying audio data: %w", err)
	}
	mw.Close()

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, c.apiBase+"/sendVoice", &buf)
	if err != nil {
		return fmt.Errorf("creating request: %w", err)
	}
	req.Header.Set("Content-Type", mw.FormDataContentType())

	resp, err := c.http.Do(req)
	if err != nil {
		return fmt.Errorf("sendVoice: %w", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(io.LimitReader(resp.Body, 64*1024))
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("sendVoice returned %d: %s", resp.StatusCode, string(body))
	}
	var result struct {
		OK bool `json:"ok"`
	}
	if err := json.Unmarshal(body, &result); err != nil {
		return fmt.Errorf("parsing sendVoice response: %w", err)
	}
	if !result.OK {
		return fmt.Errorf("sendVoice returned ok=false: %s", string(body))
	}
	return nil
}

// ConvertToOgg converts an audio file to OGG/Opus (48kHz, 32kbps, mono).
// Returns the output path. Caller must remove it when done.
func ConvertToOgg(inputPath string) (string, error) {
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		return "", fmt.Errorf("ffmpeg not found in PATH: brew install ffmpeg")
	}
	outPath := strings.TrimSuffix(inputPath, filepath.Ext(inputPath)) + "_telegram.ogg"
	var stderr bytes.Buffer
	cmd := exec.Command("ffmpeg", "-y",
		"-i", inputPath,
		"-c:a", "libopus",
		"-b:a", "32k",
		"-ar", "48000",
		"-ac", "1",
		"-f", "ogg",
		outPath,
	)
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		return "", fmt.Errorf("ffmpeg: %w — %s", err, stderr.String())
	}
	return outPath, nil
}

// GenerateWaveform returns a 64-sample waveform (0–127 per byte) from raw PCM audio.
func GenerateWaveform(audioPath string) ([]byte, error) {
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		return nil, fmt.Errorf("ffmpeg not found in PATH: brew install ffmpeg")
	}
	cmd := exec.Command("ffmpeg",
		"-i", audioPath,
		"-f", "s16le", "-ar", "8000", "-ac", "1",
		"-",
	)
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		return nil, fmt.Errorf("ffmpeg decode: %w — %s", err, stderr.String())
	}
	pcm := stdout.Bytes()
	if len(pcm) < 2 {
		return nil, fmt.Errorf("no audio data from %s", audioPath)
	}
	numSamples := len(pcm) / 2
	const numSegments = 64
	segSize := numSamples / numSegments
	if segSize == 0 {
		segSize = 1
	}
	waveform := make([]byte, numSegments)
	for i := range waveform {
		start := i * segSize
		end := start + segSize
		if end > numSamples {
			end = numSamples
		}
		var sum float64
		for j := start; j < end; j++ {
			sample := int16(binary.LittleEndian.Uint16(pcm[j*2 : j*2+2]))
			sum += math.Abs(float64(sample))
		}
		if count := end - start; count > 0 {
			scaled := int(sum / float64(count) * 127.0 / 32767.0)
			if scaled > 127 {
				scaled = 127
			}
			waveform[i] = byte(scaled)
		}
	}
	return waveform, nil
}

// audioDuration returns file duration in seconds using ffprobe.
func audioDuration(path string) (float64, error) {
	if _, err := exec.LookPath("ffprobe"); err != nil {
		return 0, fmt.Errorf("ffprobe not found in PATH: brew install ffmpeg")
	}
	out, err := exec.Command("ffprobe",
		"-v", "quiet",
		"-print_format", "json",
		"-show_entries", "format=duration",
		path,
	).Output()
	if err != nil {
		return 0, fmt.Errorf("ffprobe: %w", err)
	}
	var result struct {
		Format struct {
			Duration string `json:"duration"`
		} `json:"format"`
	}
	if err := json.Unmarshal(out, &result); err != nil {
		return 0, fmt.Errorf("parsing ffprobe output: %w", err)
	}
	return strconv.ParseFloat(result.Format.Duration, 64)
}
