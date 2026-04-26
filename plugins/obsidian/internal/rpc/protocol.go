// Package rpc implements a Unix-socket JSON-RPC server for the Obsidian plugin.
// Requests and responses are newline-delimited JSON; each connection is
// sequential (one request at a time) with a 5-second per-request deadline.
package rpc

import "encoding/json"

// Error code constants.
const (
	ErrCodeBadRequest    = 1
	ErrCodeUnknownMethod = 2
	ErrCodeInternal      = 3
)

// Request is the incoming wire envelope.
type Request struct {
	Method string          `json:"method"`
	Params json.RawMessage `json:"params"`
}

// Response is the outgoing wire envelope.
type Response struct {
	OK     bool            `json:"ok"`
	Result json.RawMessage `json:"result,omitempty"`
	Error  *RPCError       `json:"error,omitempty"`
}

// RPCError carries a machine-readable code and a human message.
type RPCError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

// RecallRequest is the params payload for the Recall method.
type RecallRequest struct {
	Seed    string `json:"seed"`
	MaxHops int    `json:"max_hops"`
	Limit   int    `json:"limit"`
}

// StatResponse is the result payload for IndexStat.
type StatResponse struct {
	NoteCount   int `json:"note_count"`
	VertexCount int `json:"vertex_count"`
	EdgeCount   int `json:"edge_count"`
}

// RecallResponse is the result payload for Recall.
type RecallResponse struct {
	Paths []string `json:"paths"`
}

// okResponse wraps a result value in a successful Response.
func okResponse(v any) Response {
	b, _ := json.Marshal(v)
	return Response{OK: true, Result: b}
}

// errResponse wraps an error code and message in a failed Response.
func errResponse(code int, msg string) Response {
	return Response{OK: false, Error: &RPCError{Code: code, Message: msg}}
}
