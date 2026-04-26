package rpc

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/graph"
	"github.com/joeyhipolito/nanika-obsidian/internal/index"
	"github.com/joeyhipolito/nanika-obsidian/internal/recall"
)

const (
	maxLineBytes   = 1 << 20 // 1 MiB
	requestTimeout = 5 * time.Second
)

// Config holds the backend dependencies for the Server.
// All fields are optional; handlers degrade gracefully when absent.
// Graph is a closure so the server always sees the current graph after a rebuild.
// Recall is a closure that runs a scored BFS recall query; nil disables recall.
type Config struct {
	Store  *index.Store
	Graph  func() *graph.Graph
	Recall func(recall.Request) ([]recall.WalkResult, error)
}

// ServerStats is a point-in-time snapshot of server metrics.
type ServerStats struct {
	SockPath      string
	ActiveConns   int64
	RequestsTotal int64
	ErrorsTotal   int64
}

// Server is a Unix-socket JSON-RPC server.
type Server struct {
	cfg      Config
	mu       sync.Mutex
	listener net.Listener
	sockPath string // canonical path (may be a symlink)
	tempDir  string // non-empty when a short-path fallback was used
	started  bool
	wg       sync.WaitGroup

	activeConns int64 // atomic
	reqTotal    int64 // atomic
	errTotal    int64 // atomic
}

// Stats returns a point-in-time snapshot of server metrics.
func (s *Server) Stats() ServerStats {
	s.mu.Lock()
	sp := s.sockPath
	s.mu.Unlock()
	return ServerStats{
		SockPath:      sp,
		ActiveConns:   atomic.LoadInt64(&s.activeConns),
		RequestsTotal: atomic.LoadInt64(&s.reqTotal),
		ErrorsTotal:   atomic.LoadInt64(&s.errTotal),
	}
}

// New constructs a Server from cfg.
func New(cfg Config) *Server {
	return &Server{cfg: cfg}
}

// Start binds a Unix socket at sockPath (0700), removing any stale file first.
// If the path exceeds the OS limit for Unix sockets (103 bytes on macOS, 107 on
// Linux), the socket is created at a short temp path and sockPath is replaced
// with a symlink so Dial(sockPath) continues to work.
// Returns an error if the server is already started.
func (s *Server) Start(sockPath string) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.started {
		return errors.New("rpc: server already started")
	}

	_ = os.Remove(sockPath)

	ln, tempDir, err := listenUnix(sockPath)
	if err != nil {
		return fmt.Errorf("rpc: listen %s: %w", sockPath, err)
	}

	s.listener = ln
	s.sockPath = sockPath
	s.tempDir = tempDir
	s.started = true

	// Count the serve goroutine itself so Shutdown's Wait() cannot return while
	// serve is still in Accept — which would race with Add(1) for new connections.
	s.wg.Add(1)
	go s.serve()
	return nil
}

// listenUnix creates a 0700 Unix socket at path. If bind returns EINVAL (path
// too long on macOS), it retries with a short temp path and leaves a symlink at
// path pointing to the actual socket. tempDir is the directory to clean up on
// Shutdown; it is empty when the direct path was used.
func listenUnix(sockPath string) (net.Listener, string, error) {
	ln, err := net.Listen("unix", sockPath)
	if err == nil {
		if chErr := os.Chmod(sockPath, 0o700); chErr != nil {
			_ = ln.Close()
			return nil, "", chErr
		}
		return ln, "", nil
	}

	// Check if the error is EINVAL (path too long on macOS / Linux).
	if !errors.Is(err, syscall.EINVAL) {
		return nil, "", err
	}

	// Path too long: create socket in a short temp dir and symlink.
	td, tdErr := os.MkdirTemp("", "obs")
	if tdErr != nil {
		return nil, "", fmt.Errorf("mktemp fallback: %w", tdErr)
	}

	shortPath := filepath.Join(td, "s")
	ln, err = net.Listen("unix", shortPath)
	if err != nil {
		_ = os.RemoveAll(td)
		return nil, "", err
	}

	if chErr := os.Chmod(shortPath, 0o700); chErr != nil {
		_ = ln.Close()
		_ = os.RemoveAll(td)
		return nil, "", chErr
	}

	if syErr := os.Symlink(shortPath, sockPath); syErr != nil {
		_ = ln.Close()
		_ = os.RemoveAll(td)
		return nil, "", syErr
	}

	return ln, td, nil
}

// Shutdown closes the listener, waits for in-flight handlers to finish (or ctx
// to expire), then removes the socket file (and any short-path temp dir).
func (s *Server) Shutdown(ctx context.Context) error {
	s.mu.Lock()
	if !s.started {
		s.mu.Unlock()
		return nil
	}
	ln := s.listener
	sockPath := s.sockPath
	tempDir := s.tempDir
	s.started = false
	s.mu.Unlock()

	_ = ln.Close()

	done := make(chan struct{})
	go func() {
		s.wg.Wait()
		close(done)
	}()

	cleanup := func() {
		_ = os.Remove(sockPath)
		if tempDir != "" {
			_ = os.RemoveAll(tempDir)
		}
	}

	select {
	case <-done:
		cleanup()
		return nil
	case <-ctx.Done():
		cleanup()
		return ctx.Err()
	}
}

func (s *Server) serve() {
	defer s.wg.Done()
	for {
		conn, err := s.listener.Accept()
		if err != nil {
			return
		}
		s.wg.Add(1)
		go s.handleConn(conn)
	}
}

func (s *Server) handleConn(conn net.Conn) {
	defer s.wg.Done()
	defer conn.Close()
	atomic.AddInt64(&s.activeConns, 1)
	defer atomic.AddInt64(&s.activeConns, -1)

	scanner := bufio.NewScanner(conn)
	scanner.Buffer(make([]byte, maxLineBytes), maxLineBytes)

	for {
		// Per-request deadline covers the read, dispatch, and write.
		if err := conn.SetDeadline(time.Now().Add(requestTimeout)); err != nil {
			return
		}

		if !scanner.Scan() {
			return
		}

		resp := s.dispatch(scanner.Bytes())
		atomic.AddInt64(&s.reqTotal, 1)
		if !resp.OK {
			atomic.AddInt64(&s.errTotal, 1)
		}

		out, err := json.Marshal(resp)
		if err != nil {
			return
		}
		out = append(out, '\n')
		if _, err := conn.Write(out); err != nil {
			return
		}
	}
}

// Client is a synchronous client for a single Server connection.
// It is safe for concurrent use — a mutex serializes calls on the connection.
type Client struct {
	conn net.Conn
	mu   sync.Mutex
	enc  *json.Encoder
	dec  *json.Decoder
}

// Dial connects to the Server at sockPath and returns a ready Client.
// If sockPath is a symlink (created by Start when the path exceeds the OS
// sockaddr_un limit), we read one level of the link so that connect(2) uses
// the shorter actual socket path rather than the long symlink path.
func Dial(sockPath string) (*Client, error) {
	dialPath := sockPath
	if target, err := os.Readlink(sockPath); err == nil {
		dialPath = target
	}
	conn, err := net.Dial("unix", dialPath)
	if err != nil {
		return nil, fmt.Errorf("rpc: dial %s: %w", sockPath, err)
	}
	return &Client{
		conn: conn,
		enc:  json.NewEncoder(conn),
		dec:  json.NewDecoder(conn),
	}, nil
}

// Close tears down the underlying connection.
func (c *Client) Close() error {
	return c.conn.Close()
}

// Ping sends a no-op request to verify the server is alive.
func (c *Client) Ping(ctx context.Context) error {
	return c.call(ctx, "ping", struct{}{}, nil)
}

// IndexStat returns note/vertex/edge counts from the server's backends.
func (c *Client) IndexStat(ctx context.Context) (*StatResponse, error) {
	var stat StatResponse
	if err := c.call(ctx, "index_stat", struct{}{}, &stat); err != nil {
		return nil, err
	}
	return &stat, nil
}

// Recall runs a BFS-bounded link traversal from req.Seed.
func (c *Client) Recall(ctx context.Context, req RecallRequest) (*RecallResponse, error) {
	var resp RecallResponse
	if err := c.call(ctx, "recall", req, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// call is the shared request/response transport. Callers hold c.mu while active.
func (c *Client) call(ctx context.Context, method string, params any, result any) error {
	c.mu.Lock()
	defer c.mu.Unlock()

	if deadline, ok := ctx.Deadline(); ok {
		_ = c.conn.SetDeadline(deadline)
		defer func() { _ = c.conn.SetDeadline(time.Time{}) }()
	}

	paramBytes, err := json.Marshal(params)
	if err != nil {
		return fmt.Errorf("rpc: marshal params for %s: %w", method, err)
	}

	if err := c.enc.Encode(Request{Method: method, Params: paramBytes}); err != nil {
		return fmt.Errorf("rpc: send %s: %w", method, err)
	}

	var resp Response
	if err := c.dec.Decode(&resp); err != nil {
		return fmt.Errorf("rpc: recv %s: %w", method, err)
	}

	if !resp.OK {
		if resp.Error != nil {
			return fmt.Errorf("rpc: %s error %d: %s", method, resp.Error.Code, resp.Error.Message)
		}
		return fmt.Errorf("rpc: %s: server error", method)
	}

	if result != nil && resp.Result != nil {
		if err := json.Unmarshal(resp.Result, result); err != nil {
			return fmt.Errorf("rpc: decode %s result: %w", method, err)
		}
	}
	return nil
}
