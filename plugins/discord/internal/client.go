// Package internal provides a minimal Discord client for voice message operations.
package internal

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"math"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

const defaultAPIBase = "https://discord.com/api/v10"

// Client is a minimal Discord API client for voice message operations.
type Client struct {
	token   string
	http    *http.Client
	apiBase string
}

// NewClient creates a Discord client authenticated with the given bot token.
func NewClient(token string) *Client {
	return &Client{
		token:   token,
		http:    &http.Client{Timeout: 30 * time.Second},
		apiBase: defaultAPIBase,
	}
}

// Config holds the Discord plugin configuration, read from ~/.alluka/channels/discord.json.
type Config struct {
	BotToken   string   `json:"bot_token"`
	ChannelIDs []string `json:"channel_ids"`
}

// LoadConfig reads the Discord config from ~/.alluka/channels/discord.json.
func LoadConfig() (*Config, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil, fmt.Errorf("getting home dir: %w", err)
	}
	path := filepath.Join(home, ".alluka", "channels", "discord.json")
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

// ConvertToOgg converts an audio file to OGG/Opus (48kHz, 32kbps, mono).
// Returns the output path. Caller must remove it when done.
func ConvertToOgg(inputPath string) (string, error) {
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		return "", fmt.Errorf("ffmpeg not found in PATH: brew install ffmpeg")
	}
	outPath := strings.TrimSuffix(inputPath, filepath.Ext(inputPath)) + "_discord.ogg"
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

// GenerateWaveform returns a 64-sample base64 waveform string (0–127 per byte)
// by decoding the audio to raw PCM and computing mean absolute amplitude per segment.
func GenerateWaveform(audioPath string) (string, error) {
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		return "", fmt.Errorf("ffmpeg not found in PATH: brew install ffmpeg")
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
		return "", fmt.Errorf("ffmpeg decode: %w — %s", err, stderr.String())
	}
	pcm := stdout.Bytes()
	if len(pcm) < 2 {
		return "", fmt.Errorf("no audio data from %s", audioPath)
	}
	numSamples := len(pcm) / 2
	const numSegments = 64
	segSize := numSamples / numSegments
	if segSize == 0 {
		segSize = 1
	}
	means := make([]float64, numSegments)
	for i := range means {
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
			means[i] = sum / float64(count)
		}
	}
	var peak float64
	for _, m := range means {
		if m > peak {
			peak = m
		}
	}
	if peak == 0 {
		peak = 1
	}
	waveform := make([]byte, numSegments)
	for i, m := range means {
		scaled := int(m / peak * 127.0)
		if scaled > 127 {
			scaled = 127
		}
		waveform[i] = byte(scaled)
	}
	return base64.StdEncoding.EncodeToString(waveform), nil
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

// apiCall performs a Discord API call with JSON payload.
func (c *Client) apiCall(ctx context.Context, method, path string, payload, result any) error {
	body, err := json.Marshal(payload)
	if err != nil {
		return fmt.Errorf("marshal: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, method, c.apiBase+path, bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("creating request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Authorization", "Bot "+c.token)
	resp, err := c.http.Do(req)
	if err != nil {
		return fmt.Errorf("%s %s: %w", method, path, err)
	}
	defer resp.Body.Close()
	data, _ := io.ReadAll(io.LimitReader(resp.Body, 64*1024))
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("%s %s returned %d: %s", method, path, resp.StatusCode, string(data))
	}
	if result != nil {
		if err := json.Unmarshal(data, result); err != nil {
			return fmt.Errorf("parsing response: %w", err)
		}
	}
	return nil
}

// SendMessage sends a plain text message to a Discord channel.
func (c *Client) SendMessage(ctx context.Context, channelID, content string) error {
	return c.apiCall(ctx, http.MethodPost,
		"/channels/"+channelID+"/messages",
		map[string]any{"content": content},
		nil,
	)
}

// SendVoiceMessage sends an audio file as a native Discord voice message.
//
// Three-step flow:
//  1. Convert to OGG/Opus (48kHz, 32kbps, mono)
//  2. Generate 64-sample waveform + get duration
//  3. Request upload URL → PUT binary → POST message with flags:8192
func (c *Client) SendVoiceMessage(ctx context.Context, channelID, audioPath string) error {
	oggPath, err := ConvertToOgg(audioPath)
	if err != nil {
		return fmt.Errorf("converting audio: %w", err)
	}
	defer os.Remove(oggPath)

	waveform, err := GenerateWaveform(oggPath)
	if err != nil {
		return fmt.Errorf("generating waveform: %w", err)
	}

	duration, err := audioDuration(oggPath)
	if err != nil {
		return fmt.Errorf("getting duration: %w", err)
	}

	info, err := os.Stat(oggPath)
	if err != nil {
		return fmt.Errorf("stat ogg: %w", err)
	}

	// Step 1: request upload URL.
	type fileEntry struct {
		ID       string `json:"id"`
		Filename string `json:"filename"`
		FileSize int64  `json:"file_size"`
	}
	var uploadResp struct {
		Attachments []struct {
			UploadURL      string `json:"upload_url"`
			UploadFilename string `json:"upload_filename"`
		} `json:"attachments"`
	}
	if err := c.apiCall(ctx, http.MethodPost,
		"/channels/"+channelID+"/attachments",
		map[string]any{"files": []fileEntry{{ID: "0", Filename: "voice-message.ogg", FileSize: info.Size()}}},
		&uploadResp,
	); err != nil {
		return fmt.Errorf("requesting upload URL: %w", err)
	}
	if len(uploadResp.Attachments) == 0 {
		return fmt.Errorf("no upload URL returned by Discord")
	}
	uploadURL := uploadResp.Attachments[0].UploadURL
	uploadFilename := uploadResp.Attachments[0].UploadFilename

	// Step 2: PUT binary to GCS upload URL.
	f, err := os.Open(oggPath)
	if err != nil {
		return fmt.Errorf("opening ogg: %w", err)
	}
	defer f.Close()
	putReq, err := http.NewRequestWithContext(ctx, http.MethodPut, uploadURL, f)
	if err != nil {
		return fmt.Errorf("creating PUT request: %w", err)
	}
	putReq.ContentLength = info.Size()
	putReq.Header.Set("Content-Type", "audio/ogg")
	putResp, err := c.http.Do(putReq)
	if err != nil {
		return fmt.Errorf("uploading audio: %w", err)
	}
	defer putResp.Body.Close()
	io.Copy(io.Discard, putResp.Body) //nolint:errcheck
	if putResp.StatusCode < 200 || putResp.StatusCode >= 300 {
		return fmt.Errorf("upload returned HTTP %d", putResp.StatusCode)
	}

	// Step 3: POST message with IS_VOICE_MESSAGE flag (8192 = 1<<13).
	type attachment struct {
		ID               string  `json:"id"`
		Filename         string  `json:"filename"`
		UploadedFilename string  `json:"uploaded_filename"`
		DurationSecs     float64 `json:"duration_secs"`
		Waveform         string  `json:"waveform"`
	}
	if err := c.apiCall(ctx, http.MethodPost,
		"/channels/"+channelID+"/messages",
		map[string]any{
			"flags": 8192,
			"attachments": []attachment{{
				ID:               "0",
				Filename:         "voice-message.ogg",
				UploadedFilename: uploadFilename,
				DurationSecs:     duration,
				Waveform:         waveform,
			}},
		},
		nil,
	); err != nil {
		return fmt.Errorf("sending message: %w", err)
	}
	return nil
}
