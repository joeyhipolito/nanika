// nen-mcp is a stdio JSON-RPC MCP server for the nen ability system.
// It implements the Model Context Protocol (2024-11-05) transport over stdin/stdout.
// Currently exposes an empty tool set; tools will be added as nen capabilities are surfaced.
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"os"
)

const (
	protocolVersion = "2024-11-05"
	serverName      = "nen-mcp"
	serverVersion   = "0.1.0"
)

// rpcRequest is the inbound JSON-RPC 2.0 envelope.
type rpcRequest struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      json.RawMessage `json:"id,omitempty"` // null for notifications
	Method  string          `json:"method"`
	Params  json.RawMessage `json:"params,omitempty"`
}

// rpcResponse is the outbound JSON-RPC 2.0 envelope.
type rpcResponse struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      json.RawMessage `json:"id"`
	Result  any             `json:"result,omitempty"`
	Error   *rpcError       `json:"error,omitempty"`
}

type rpcError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

// MCP result types

type initializeResult struct {
	ProtocolVersion string             `json:"protocolVersion"`
	Capabilities    serverCapabilities `json:"capabilities"`
	ServerInfo      serverInfo         `json:"serverInfo"`
}

type serverCapabilities struct {
	Tools *toolsCapability `json:"tools,omitempty"`
}

type toolsCapability struct{}

type serverInfo struct {
	Name    string `json:"name"`
	Version string `json:"version"`
}

type toolsListResult struct {
	Tools []tool `json:"tools"`
}

type tool struct {
	Name        string `json:"name"`
	Description string `json:"description"`
	InputSchema any    `json:"inputSchema"`
}

func main() {
	// MCP over stdio: diagnostics must go to stderr only.
	log.SetOutput(os.Stderr)
	log.SetFlags(0)

	// Handle non-MCP subcommands before entering the stdio loop.
	if len(os.Args) >= 2 {
		switch os.Args[1] {
		case "doctor":
			jsonMode := len(os.Args) >= 3 && os.Args[2] == "--json"
			runDoctor(jsonMode)
			return
		case "--version", "version":
			fmt.Printf("nen-mcp %s (MCP protocol %s)\n", serverVersion, protocolVersion)
			return
		case "--help", "help", "-h":
			fmt.Println("Usage:")
			fmt.Println("  nen-mcp              Start MCP stdio server")
			fmt.Println("  nen-mcp doctor       Check backing stores and list tools")
			fmt.Println("  nen-mcp doctor --json  Same, as JSON")
			fmt.Println("  nen-mcp version      Print version and protocol")
			return
		}
	}

	if err := runLoop(os.Stdin, os.Stdout); err != nil {
		log.Printf("stdin scanner: %v", err)
		os.Exit(1)
	}
}

// runLoop reads newline-delimited JSON-RPC 2.0 requests from r and writes
// responses to w until r reaches EOF. Extracted for testability.
func runLoop(r io.Reader, w io.Writer) error {
	enc := json.NewEncoder(w)
	scanner := bufio.NewScanner(r)
	// MCP messages can be large; allow up to 4 MiB per line.
	scanner.Buffer(make([]byte, 4*1024*1024), 4*1024*1024)

	for scanner.Scan() {
		line := scanner.Bytes()
		if len(line) == 0 {
			continue
		}

		var req rpcRequest
		if err := json.Unmarshal(line, &req); err != nil {
			// Malformed JSON — send parse error if we can't even get an ID.
			writeError(enc, nil, -32700, "parse error")
			continue
		}

		// Notifications have no ID; handle them without sending a response.
		if req.ID == nil || string(req.ID) == "null" {
			handleNotification(req.Method)
			continue
		}

		resp := dispatch(req)
		if err := enc.Encode(resp); err != nil {
			log.Printf("encode response: %v", err)
		}
	}

	if err := scanner.Err(); err != nil && err != io.EOF {
		return err
	}
	return nil
}

func dispatch(req rpcRequest) rpcResponse {
	switch req.Method {
	case "initialize":
		return rpcResponse{
			JSONRPC: "2.0",
			ID:      req.ID,
			Result: initializeResult{
				ProtocolVersion: protocolVersion,
				Capabilities:    serverCapabilities{Tools: &toolsCapability{}},
				ServerInfo:      serverInfo{Name: serverName, Version: serverVersion},
			},
		}

	case "tools/list":
		return rpcResponse{
			JSONRPC: "2.0",
			ID:      req.ID,
			Result:  listTools(),
		}

	case "tools/call":
		return callTool(req)

	default:
		return rpcResponse{
			JSONRPC: "2.0",
			ID:      req.ID,
			Error:   &rpcError{Code: -32601, Message: fmt.Sprintf("method not found: %s", req.Method)},
		}
	}
}

func handleNotification(method string) {
	// notifications/initialized is the only expected notification at startup.
	// Log unknown notifications to stderr for debugging; otherwise ignore.
	if method != "notifications/initialized" {
		log.Printf("unhandled notification: %s", method)
	}
}

func writeError(enc *json.Encoder, id json.RawMessage, code int, message string) {
	resp := rpcResponse{
		JSONRPC: "2.0",
		ID:      id,
		Error:   &rpcError{Code: code, Message: message},
	}
	if err := enc.Encode(resp); err != nil {
		log.Printf("encode error response: %v", err)
	}
}
