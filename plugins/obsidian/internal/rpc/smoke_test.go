package rpc

import (
	"encoding/json"
	"testing"
	"time"
)

// smokeTransact sends a raw JSON line to sock and returns the parsed Response.
// Uses Dial (which resolves symlinks for long paths on macOS) rather than raw
// net.DialTimeout so it works on both direct and symlinked socket paths.
func smokeTransact(t *testing.T, sock, msg string) Response {
	t.Helper()
	c, err := Dial(sock)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer c.Close()
	c.conn.SetDeadline(time.Now().Add(3 * time.Second))
	c.conn.Write([]byte(msg + "\n"))
	buf := make([]byte, 4096)
	n, err := c.conn.Read(buf)
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	var resp Response
	if err := json.Unmarshal(buf[:n], &resp); err != nil {
		t.Fatalf("unmarshal: %v (raw: %q)", err, string(buf[:n]))
	}
	return resp
}

// TestSmoke_G_HappyPathRecall — null Recall backend returns ok=true + empty paths.
func TestSmoke_G_HappyPathRecall(t *testing.T) {
	_, sock := newTestServer(t)
	resp := smokeTransact(t, sock, `{"method":"recall","params":{"seed":"notes/index.md","max_hops":2,"limit":10}}`)
	if !resp.OK {
		t.Fatalf("G: expected ok=true, got ok=false error=%+v", resp.Error)
	}
	var r RecallResponse
	json.Unmarshal(resp.Result, &r)
	if r.Paths == nil {
		t.Fatalf("G: paths must not be nil")
	}
}

// TestSmoke_H_EmptySeedError — empty seed returns ok=true with zero paths (graceful).
func TestSmoke_H_EmptySeedError(t *testing.T) {
	_, sock := newTestServer(t)
	resp := smokeTransact(t, sock, `{"method":"recall","params":{"seed":"","max_hops":2,"limit":10}}`)
	if !resp.OK {
		t.Fatalf("H: expected graceful ok=true for empty seed, got error: %+v", resp.Error)
	}
	var r RecallResponse
	json.Unmarshal(resp.Result, &r)
	if len(r.Paths) != 0 {
		t.Fatalf("H: empty seed must return 0 paths, got %d", len(r.Paths))
	}
}

// TestSmoke_I_ZeroLimitError — limit=0 returns ok=true with empty paths (no panic).
func TestSmoke_I_ZeroLimitError(t *testing.T) {
	_, sock := newTestServer(t)
	resp := smokeTransact(t, sock, `{"method":"recall","params":{"seed":"notes/index.md","max_hops":2,"limit":0}}`)
	if !resp.OK {
		t.Fatalf("I: expected ok=true for limit=0, got error: %+v", resp.Error)
	}
}

// TestSmoke_J_LimitClamping — very large limit returns ok=true (no overflow/panic).
func TestSmoke_J_LimitClamping(t *testing.T) {
	_, sock := newTestServer(t)
	resp := smokeTransact(t, sock, `{"method":"recall","params":{"seed":"notes/index.md","max_hops":2,"limit":9999}}`)
	if !resp.OK {
		t.Fatalf("J: expected ok=true for limit=9999, got error: %+v", resp.Error)
	}
	var r RecallResponse
	json.Unmarshal(resp.Result, &r)
	if len(r.Paths) != 0 {
		t.Fatalf("J: nil backend must return 0 paths regardless of limit, got %d", len(r.Paths))
	}
}
