package main

import (
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"sync/atomic"
	"syscall"
	"time"
)

const (
	pluginID     = "go-stub"
	protoVersion = "1.0.0"
)

var seqN uint64
func nextSeq() uint64 { return atomic.AddUint64(&seqN, 1) }
func nowTS() string   { return time.Now().UTC().Format("2006-01-02T15:04:05.000") + "Z" }

// Envelope covers all five protocol kinds (request/response/event/heartbeat/shutdown).
// The Kind field discriminates; omitempty drops irrelevant fields per kind.
type Envelope struct {
	Kind     string          `json:"kind"`
	ID       string          `json:"id,omitempty"`
	Method   string          `json:"method,omitempty"`
	Result   json.RawMessage `json:"result,omitempty"`
	Error    *ErrorObj       `json:"error,omitempty"`
	Type     string          `json:"type,omitempty"`
	TS       string          `json:"ts,omitempty"`
	Sequence *uint64         `json:"sequence,omitempty"`
	Data     json.RawMessage `json:"data,omitempty"`
}
type ErrorObj struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}
type Manifest struct {
	Name         string  `json:"name"`
	Version      string  `json:"version"`
	Description  string  `json:"description"`
	Capabilities []Cap   `json:"capabilities"`
	Icon         *string `json:"icon"`
}
type Cap struct {
	Kind   string `json:"kind"`
	Prefix string `json:"prefix,omitempty"`
}

var manifest = Manifest{
	Name:         "Go Stub",
	Version:      "0.1.0",
	Description:  "Minimal Go stub implementing the dust v1 handshake.",
	Capabilities: []Cap{{Kind: "command", Prefix: "go-stub"}},
}

// readMsg reads one length-prefixed frame: 4-byte big-endian u32 then payload.
func readMsg(r io.Reader) ([]byte, error) {
	var lb [4]byte
	if _, err := io.ReadFull(r, lb[:]); err != nil {
		return nil, err
	}
	n := binary.BigEndian.Uint32(lb[:])
	if n == 0 {
		return nil, io.EOF
	}
	buf := make([]byte, n)
	_, err := io.ReadFull(r, buf)
	return buf, err
}
func writeMsg(w io.Writer, env Envelope) error {
	data, err := json.Marshal(env)
	if err != nil {
		return err
	}
	var lb [4]byte
	binary.BigEndian.PutUint32(lb[:], uint32(len(data)))
	if _, err := w.Write(lb[:]); err != nil {
		return err
	}
	_, err = w.Write(data)
	return err
}
func dispatch(req Envelope) Envelope {
	switch req.Method {
	case "manifest", "refresh_manifest":
		data, _ := json.Marshal(manifest)
		return Envelope{Kind: "response", ID: req.ID, Result: json.RawMessage(data)}
	case "render":
		return Envelope{Kind: "response", ID: req.ID, Result: json.RawMessage(`[]`)}
	case "action":
		return Envelope{Kind: "response", ID: req.ID, Result: json.RawMessage(`{"success":true}`)}
	default:
		return Envelope{
			Kind:  "response",
			ID:    req.ID,
			Error: &ErrorObj{Code: -32601, Message: "method not found: " + req.Method},
		}
	}
}
func handle(conn net.Conn) {
	defer conn.Close()
	seq := nextSeq()
	mfData, _ := json.Marshal(manifest)
	readyData, _ := json.Marshal(map[string]interface{}{
		"manifest":         json.RawMessage(mfData),
		"protocol_version": protoVersion,
		"plugin_info":      map[string]interface{}{"pid": os.Getpid(), "started_at": nowTS()},
	})
	if err := writeMsg(conn, Envelope{
		Kind: "event", ID: fmt.Sprintf("evt_%016x", seq),
		Type: "ready", TS: nowTS(), Sequence: &seq, Data: json.RawMessage(readyData),
	}); err != nil {
		fmt.Fprintf(os.Stderr, "dust: ready send: %v\n", err)
		return
	}
	if err := conn.SetDeadline(time.Now().Add(5 * time.Second)); err != nil {
		return
	}
	raw, err := readMsg(conn)
	if err != nil {
		fmt.Fprintf(os.Stderr, "dust: host_info read: %v\n", err)
		return
	}
	var hi Envelope
	if err := json.Unmarshal(raw, &hi); err != nil || hi.Kind != "event" || hi.Type != "host_info" {
		fmt.Fprintf(os.Stderr, "dust: expected host_info, got kind=%q type=%q\n", hi.Kind, hi.Type)
		return
	}
	conn.SetDeadline(time.Time{}) //nolint:errcheck
	for {
		raw, err := readMsg(conn)
		if err != nil {
			return
		}
		var env Envelope
		if err := json.Unmarshal(raw, &env); err != nil {
			fmt.Fprintf(os.Stderr, "dust: parse: %v\n", err)
			continue
		}
		switch env.Kind {
		case "request":
			if err := writeMsg(conn, dispatch(env)); err != nil {
				return
			}
		case "heartbeat":
			if err := writeMsg(conn, Envelope{Kind: "heartbeat", TS: nowTS()}); err != nil {
				return
			}
		case "shutdown":
			return
		}
	}
}
func runtimeDir() (string, error) {
	if xdg := os.Getenv("XDG_RUNTIME_DIR"); xdg != "" {
		return filepath.Join(xdg, "nanika", "plugins"), nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("HOME not set: %w", err)
	}
	return filepath.Join(home, ".alluka", "run", "plugins"), nil
}
func main() {
	dir, err := runtimeDir()
	if err != nil {
		fmt.Fprintln(os.Stderr, "dust:", err)
		os.Exit(1)
	}
	if err := os.MkdirAll(dir, 0o755); err != nil {
		fmt.Fprintln(os.Stderr, "dust:", err)
		os.Exit(1)
	}
	sockPath := filepath.Join(dir, pluginID+".sock")
	os.Remove(sockPath)
	ln, err := net.Listen("unix", sockPath)
	if err != nil {
		fmt.Fprintln(os.Stderr, "dust:", err)
		os.Exit(1)
	}
	sigs := make(chan os.Signal, 1)
	signal.Notify(sigs, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sigs
		ln.Close()
		os.Remove(sockPath)
		os.Exit(0)
	}()
	fmt.Fprintf(os.Stderr, "dust: %s listening on %s\n", pluginID, sockPath)
	for {
		conn, err := ln.Accept()
		if err != nil {
			return
		}
		go handle(conn)
	}
}
