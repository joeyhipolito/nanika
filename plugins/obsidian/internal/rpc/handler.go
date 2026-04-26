package rpc

import (
	"encoding/json"

	"github.com/joeyhipolito/nanika-obsidian/internal/recall"
)

// dispatch decodes a raw request line and routes to the appropriate handler.
// Unknown methods and malformed payloads produce error responses, not panics.
func (s *Server) dispatch(line []byte) Response {
	var req Request
	if err := json.Unmarshal(line, &req); err != nil {
		return errResponse(ErrCodeBadRequest, "bad request: "+err.Error())
	}

	switch req.Method {
	case "ping":
		return s.handlePing()
	case "index_stat":
		return s.handleIndexStat()
	case "recall":
		return s.handleRecall(req.Params)
	default:
		return errResponse(ErrCodeUnknownMethod, "unknown method: "+req.Method)
	}
}

func (s *Server) handlePing() Response {
	return okResponse(struct{}{})
}

func (s *Server) handleIndexStat() Response {
	stat := StatResponse{}
	if s.cfg.Store != nil {
		n, _ := s.cfg.Store.NoteCount()
		stat.NoteCount = n
	}
	if s.cfg.Graph != nil {
		if g := s.cfg.Graph(); g != nil {
			stat.VertexCount = g.VertexCount()
			stat.EdgeCount = g.EdgeCount()
		}
	}
	return okResponse(stat)
}

func (s *Server) handleRecall(params json.RawMessage) Response {
	var req RecallRequest
	if err := json.Unmarshal(params, &req); err != nil {
		return errResponse(ErrCodeBadRequest, "bad recall params: "+err.Error())
	}

	if s.cfg.Recall == nil {
		return okResponse(RecallResponse{Paths: []string{}})
	}

	results, err := s.cfg.Recall(recall.Request{
		Seed:    req.Seed,
		MaxHops: req.MaxHops,
		Limit:   req.Limit,
	})
	if err != nil {
		return errResponse(ErrCodeInternal, "recall: "+err.Error())
	}

	paths := make([]string, len(results))
	for i, r := range results {
		paths[i] = r.Path
	}
	return okResponse(RecallResponse{Paths: paths})
}
