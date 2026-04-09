package cmd

import (
	"fmt"
	"io"
	"os"
	"strconv"
	"strings"
)

func readTextInput(args []string) (string, error) {
	if len(args) > 0 {
		return strings.TrimSpace(strings.Join(args, " ")), nil
	}
	info, err := os.Stdin.Stat()
	if err == nil && info.Mode()&os.ModeCharDevice == 0 {
		data, readErr := io.ReadAll(os.Stdin)
		if readErr != nil {
			return "", fmt.Errorf("read stdin: %w", readErr)
		}
		return strings.TrimSpace(string(data)), nil
	}
	return "", nil
}

func parseKV(raw string) (string, string, error) {
	parts := strings.SplitN(raw, "=", 2)
	if len(parts) != 2 {
		return "", "", fmt.Errorf("expected key=value, got %q", raw)
	}
	key := strings.TrimSpace(parts[0])
	value := strings.TrimSpace(parts[1])
	if key == "" || value == "" {
		return "", "", fmt.Errorf("expected key=value, got %q", raw)
	}
	return key, value, nil
}

func parseKVArgs(values []string) (map[string]string, error) {
	if len(values) == 0 {
		return nil, nil
	}
	out := make(map[string]string, len(values))
	for _, raw := range values {
		key, value, err := parseKV(raw)
		if err != nil {
			return nil, err
		}
		out[key] = value
	}
	return out, nil
}

func parsePositiveInt(raw string, fallback int) (int, error) {
	if strings.TrimSpace(raw) == "" {
		return fallback, nil
	}
	n, err := strconv.Atoi(raw)
	if err != nil {
		return 0, fmt.Errorf("invalid integer %q", raw)
	}
	if n <= 0 {
		return 0, fmt.Errorf("value must be > 0")
	}
	return n, nil
}
