package daemon

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/sdk"
)

// chatSession tracks one in-flight chat query.
// tokenCh is buffered (256) so that fast Claude responses are preserved even
// when the SSE client connects slightly after the POST returns.
type chatSession struct {
	id        string
	tokenCh   chan string   // receives text chunks from OnChunk; closed when QueryText returns
	done      chan struct{} // closed after tokenCh is closed and err is set
	err       error        // non-nil on failure; set before done is closed
	createdAt time.Time
}

// Message is one chat turn (user or assistant).
type Message struct {
	Role      string    `json:"role"`
	Content   string    `json:"content"`
	CreatedAt time.Time `json:"created_at"`
}

// Conversation holds the full message history for one conversation.
type Conversation struct {
	ID        string    `json:"id"`
	Messages  []Message `json:"messages"`
	CreatedAt time.Time `json:"created_at"`
	UpdatedAt time.Time `json:"updated_at"`
}

// ConversationSummary is returned by GET /api/chat (list endpoint).
type ConversationSummary struct {
	ID            string    `json:"id"`
	MessageCount  int       `json:"message_count"`
	LastPreview   string    `json:"last_preview,omitempty"`
	LastMessageAt time.Time `json:"last_message_at,omitempty"`
	CreatedAt     time.Time `json:"created_at"`
}

// chatStore manages active chat sessions keyed by conversation_id.
// convSession maps conversation_id to Claude session_id for multi-turn continuity;
// it outlives each individual streaming session and is never swept.
// convs holds the persistent message history for each conversation.
type chatStore struct {
	mu          sync.RWMutex
	sessions    map[string]*chatSession
	convSession map[string]string        // conversation_id -> Claude session_id
	convs       map[string]*Conversation // conversation_id -> full history
}

func newChatStore() *chatStore {
	cs := &chatStore{
		sessions:    make(map[string]*chatSession),
		convSession: make(map[string]string),
		convs:       make(map[string]*Conversation),
	}
	go cs.sweep()
	return cs
}

func (cs *chatStore) storeSessionID(convID, sessionID string) {
	cs.mu.Lock()
	cs.convSession[convID] = sessionID
	cs.mu.Unlock()
}

func (cs *chatStore) getSessionID(convID string) string {
	cs.mu.RLock()
	id := cs.convSession[convID]
	cs.mu.RUnlock()
	return id
}

func (cs *chatStore) create(id string) *chatSession {
	s := &chatSession{
		id:        id,
		tokenCh:   make(chan string, 256),
		done:      make(chan struct{}),
		createdAt: time.Now(),
	}
	cs.mu.Lock()
	cs.sessions[id] = s
	cs.mu.Unlock()
	return s
}

func (cs *chatStore) get(id string) (*chatSession, bool) {
	cs.mu.RLock()
	s, ok := cs.sessions[id]
	cs.mu.RUnlock()
	return s, ok
}

func (cs *chatStore) delete(id string) {
	cs.mu.Lock()
	delete(cs.sessions, id)
	cs.mu.Unlock()
}

// sweep removes sessions older than 5 minutes to prevent unbounded map growth
// when clients POST /api/chat but never open the stream endpoint.
func (cs *chatStore) sweep() {
	t := time.NewTicker(2 * time.Minute)
	defer t.Stop()
	for range t.C {
		cutoff := time.Now().Add(-5 * time.Minute)
		cs.mu.Lock()
		for id, s := range cs.sessions {
			if s.createdAt.Before(cutoff) {
				delete(cs.sessions, id)
			}
		}
		cs.mu.Unlock()
	}
}

// addMessage appends a message to the conversation history, creating the
// conversation record if it doesn't already exist.
func (cs *chatStore) addMessage(convID, role, content string) {
	cs.mu.Lock()
	defer cs.mu.Unlock()
	conv, ok := cs.convs[convID]
	if !ok {
		conv = &Conversation{
			ID:        convID,
			Messages:  nil,
			CreatedAt: time.Now(),
		}
		cs.convs[convID] = conv
	}
	now := time.Now()
	conv.Messages = append(conv.Messages, Message{
		Role:      role,
		Content:   content,
		CreatedAt: now,
	})
	conv.UpdatedAt = now
}

// getConversation returns a copy of the conversation, or nil if not found.
func (cs *chatStore) getConversation(convID string) *Conversation {
	cs.mu.RLock()
	conv := cs.convs[convID]
	cs.mu.RUnlock()
	if conv == nil {
		return nil
	}
	cp := *conv
	cp.Messages = make([]Message, len(conv.Messages))
	copy(cp.Messages, conv.Messages)
	return &cp
}

// listConversations returns a summary of all conversations sorted by
// most-recently updated first.
func (cs *chatStore) listConversations() []ConversationSummary {
	cs.mu.RLock()
	defer cs.mu.RUnlock()
	out := make([]ConversationSummary, 0, len(cs.convs))
	for _, conv := range cs.convs {
		s := ConversationSummary{
			ID:           conv.ID,
			MessageCount: len(conv.Messages),
			CreatedAt:    conv.CreatedAt,
		}
		if len(conv.Messages) > 0 {
			last := conv.Messages[len(conv.Messages)-1]
			s.LastMessageAt = last.CreatedAt
			if len(last.Content) > 100 {
				s.LastPreview = last.Content[:100]
			} else {
				s.LastPreview = last.Content
			}
		}
		out = append(out, s)
	}
	sort.Slice(out, func(i, j int) bool {
		return out[i].LastMessageAt.After(out[j].LastMessageAt)
	})
	return out
}

// deleteConversation removes a conversation, its associated Claude session ID,
// and any in-flight streaming session.
func (cs *chatStore) deleteConversation(convID string) {
	cs.mu.Lock()
	delete(cs.convs, convID)
	delete(cs.convSession, convID)
	// Close any in-flight streaming session for this conversation.
	if sess, ok := cs.sessions[convID]; ok {
		close(sess.tokenCh)
		delete(cs.sessions, convID)
	}
	cs.mu.Unlock()
}

// chatPostRequest is the body of POST /api/chat.
type chatPostRequest struct {
	Message          string `json:"message"`
	ConversationID   string `json:"conversation_id,omitempty"`
	DashboardContext string `json:"dashboard_context,omitempty"`
}

// chatPostResponse is returned by POST /api/chat (202 Accepted).
type chatPostResponse struct {
	ConversationID string `json:"conversation_id"`
}

// handleChat handles POST /api/chat.
// It starts a Claude query in the background and returns immediately with a
// conversation_id that the client uses to open the SSE stream.
func (s *APIServer) handleChat(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)

	var req chatPostRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, "invalid request body", http.StatusBadRequest)
		return
	}
	if req.Message == "" {
		http.Error(w, "message is required", http.StatusBadRequest)
		return
	}

	convID := req.ConversationID
	if convID == "" {
		convID = generateRequestID()
	}

	// Persist user message before starting the query so history is consistent
	// even if the client never opens the stream endpoint.
	s.chat.addMessage(convID, "user", req.Message)

	sess := s.chat.create(convID)
	systemPrompt := s.buildChatSystemPrompt()
	if req.DashboardContext != "" {
		systemPrompt += "\n\n## Dashboard State\n" + req.DashboardContext
	}
	resumeSessionID := s.chat.getSessionID(convID)

	go func() {
		var buf strings.Builder
		opts := &sdk.AgentOptions{
			SystemPrompt:    systemPrompt,
			MaxTurns:        1,
			ResumeSessionID: resumeSessionID,
			OnChunk: func(chunk string) {
				buf.WriteString(chunk)
				select {
				case sess.tokenCh <- chunk:
				default:
					// Buffer full — drop chunk. At 256 capacity this only
					// happens if the client is not reading the stream.
				}
			},
			OnEvent: func(ev *sdk.StreamedEvent) {
				if ev.Kind == sdk.KindTurnEnd && ev.SessionID != "" {
					s.chat.storeSessionID(convID, ev.SessionID)
				}
			},
		}
		_, err := sdk.QueryText(context.Background(), req.Message, opts)
		if err == nil && buf.Len() > 0 {
			s.chat.addMessage(convID, "assistant", buf.String())
		}
		sess.err = err
		close(sess.tokenCh)
		close(sess.done)
	}()

	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusAccepted)
	json.NewEncoder(w).Encode(chatPostResponse{ConversationID: convID}) //nolint:errcheck
}

// handleChatStream handles GET /api/chat/{id}/stream.
// It forwards token chunks from the background Claude query as SSE events.
//
// SSE events emitted:
//
//	event: token  — data: {"text":"<chunk>"}
//	event: done   — data: {}
//	event: error  — data: {"message":"<error>"}
func (s *APIServer) handleChatStream(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)

	convID := r.PathValue("id")
	sess, ok := s.chat.get(convID)
	if !ok {
		http.Error(w, "conversation not found", http.StatusNotFound)
		return
	}

	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")
	w.Header().Set("X-Accel-Buffering", "no")

	flusher, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "streaming not supported", http.StatusInternalServerError)
		return
	}

	// Clean up the session after the client disconnects so we don't
	// accumulate sessions for conversations that were already streamed.
	defer s.chat.delete(convID)

	ctx := r.Context()
	ticker := time.NewTicker(sseKeepaliveInterval)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return

		case <-ticker.C:
			fmt.Fprintf(w, ": keepalive\n\n")
			flusher.Flush()

		case chunk, more := <-sess.tokenCh:
			if !more {
				// tokenCh closed — QueryText has returned.
				if sess.err != nil {
					// Wrap the Claude error with enough context for the client
					// to distinguish transient failures from configuration issues.
					errMsg := fmt.Sprintf("claude session failed: %s", sess.err.Error())
					data, _ := json.Marshal(map[string]string{"message": errMsg})
					fmt.Fprintf(w, "event: error\ndata: %s\n\n", data)
				} else {
					fmt.Fprintf(w, "event: done\ndata: {}\n\n")
				}
				flusher.Flush()
				return
			}
			data, _ := json.Marshal(map[string]string{"text": chunk})
			fmt.Fprintf(w, "event: token\ndata: %s\n\n", data)
			flusher.Flush()
		}
	}
}

// handleListConversations handles GET /api/chat.
// Returns all conversations sorted by most-recently updated first.
func (s *APIServer) handleListConversations(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)
	writeJSON(w, s.chat.listConversations())
}

// handleGetConversation handles GET /api/chat/{id}.
// Returns the full message history for a conversation.
func (s *APIServer) handleGetConversation(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)
	convID := r.PathValue("id")
	if convID == "" {
		http.Error(w, "conversation id required", http.StatusBadRequest)
		return
	}
	conv := s.chat.getConversation(convID)
	if conv == nil {
		http.Error(w, "conversation not found", http.StatusNotFound)
		return
	}
	writeJSON(w, conv)
}

// handleDeleteConversation handles DELETE /api/chat/{id}.
// Removes the conversation history and its associated Claude session.
func (s *APIServer) handleDeleteConversation(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)
	convID := r.PathValue("id")
	if convID == "" {
		http.Error(w, "conversation id required", http.StatusBadRequest)
		return
	}
	s.chat.deleteConversation(convID)
	w.WriteHeader(http.StatusNoContent)
}

// buildChatSystemPrompt returns a system prompt for chat sessions that includes
// live system state: running missions, recent mission events, and available commands.
func (s *APIServer) buildChatSystemPrompt() string {
	var b strings.Builder
	b.WriteString("You are the Nanika AI assistant — a personal intelligence OS built on Claude Code.\n")
	b.WriteString("Answer questions about the system or help with tasks. Be concise.\n\n")

	b.WriteString("## Active Missions\n")
	running := s.liveState.RunningMissions()
	if len(running) == 0 {
		b.WriteString("- No missions currently running\n")
	} else {
		fmt.Fprintf(&b, "- %d mission(s) running: %s\n", len(running), strings.Join(running, ", "))
	}

	b.WriteString("\n## Recent Events\n")
	allEvents := s.bus.EventsSince(0)
	// Filter to mission lifecycle events and show the most recent 10.
	var missionEvents []event.Event
	for _, ev := range allEvents {
		switch ev.Type {
		case event.MissionStarted, event.MissionCompleted, event.MissionFailed, event.MissionCancelled:
			missionEvents = append(missionEvents, ev)
		}
	}
	if len(missionEvents) == 0 {
		b.WriteString("- No recent mission events\n")
	} else {
		start := 0
		if len(missionEvents) > 10 {
			start = len(missionEvents) - 10
		}
		for _, ev := range missionEvents[start:] {
			fmt.Fprintf(&b, "- [%s] %s %s\n", ev.Timestamp.Format("15:04:05"), ev.Type, ev.MissionID)
		}
	}

	b.WriteString("\n## Available Commands\n")
	b.WriteString("- `orchestrator run <task>` — start a new mission\n")
	b.WriteString("- `orchestrator run <file.md>` — run a pre-decomposed mission file\n")
	b.WriteString("- `orchestrator status` — show running missions\n")
	b.WriteString("- `orchestrator cleanup` — remove old workspaces and worktrees\n")
	b.WriteString("- `orchestrator metrics` — show mission statistics\n")
	b.WriteString("- `orchestrator learn` — extract learnings from recent missions\n")
	b.WriteString("- `orchestrator nen scan` — run NEN security scanners\n")

	return b.String()
}
