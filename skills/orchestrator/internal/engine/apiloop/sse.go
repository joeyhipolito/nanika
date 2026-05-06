package apiloop

import (
	"bufio"
	"io"
	"strings"
)

// SSEFrame is one parsed Server-Sent Events frame: an optional event name and
// the concatenated data lines (without the "data:" prefix).
type SSEFrame struct {
	Event string
	Data  string
}

// ScanSSE iterates over the SSE stream in r, invoking visit for each parsed
// frame. visit returning false stops iteration. Returns the first scanner error
// (if any) once iteration completes.
//
// Both Anthropic and OpenAI emit SSE frames terminated by a blank line.
// Anthropic prefixes each frame with "event: <name>"; OpenAI omits the event
// line and uses only "data:" with a "[DONE]" sentinel. ScanSSE handles both.
func ScanSSE(r io.Reader, visit func(SSEFrame) bool) error {
	scanner := bufio.NewScanner(r)
	scanner.Buffer(make([]byte, 0, 64*1024), 4*1024*1024)

	var (
		eventName string
		dataLines []string
	)
	dispatch := func() bool {
		if len(dataLines) == 0 && eventName == "" {
			return true
		}
		f := SSEFrame{Event: eventName, Data: strings.Join(dataLines, "\n")}
		eventName, dataLines = "", nil
		return visit(f)
	}

	for scanner.Scan() {
		line := scanner.Text()
		if line == "" {
			if !dispatch() {
				return nil
			}
			continue
		}
		if strings.HasPrefix(line, ":") {
			continue // SSE comment
		}
		if rest, ok := strings.CutPrefix(line, "event:"); ok {
			eventName = strings.TrimSpace(rest)
			continue
		}
		if rest, ok := strings.CutPrefix(line, "data:"); ok {
			dataLines = append(dataLines, strings.TrimPrefix(strings.TrimSpace(rest), " "))
			continue
		}
	}
	if err := scanner.Err(); err != nil {
		return err
	}
	// Dispatch any trailing event without a blank-line terminator.
	dispatch()
	return nil
}
