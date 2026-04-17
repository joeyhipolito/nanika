package dream

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"strings"
)

// estimateTokens approximates the token count of text using the chars/4
// heuristic. It is intentionally rough — precision is not required here
// because the chunk budget is already a generous estimate.
func estimateTokens(text string) int {
	if text == "" {
		return 0
	}
	return (len(text) + 3) / 4
}

// ChunkSession splits session messages into non-overlapping, token-bounded
// windows. Each window becomes one LLM extraction call. Messages are never
// split mid-message; a message that exceeds the budget on its own is placed
// in its own chunk (rather than dropped).
func ChunkSession(sess Session, maxTokens int) []Chunk {
	if maxTokens <= 0 {
		maxTokens = 6000
	}

	var chunks []Chunk
	var current []ConvMessage
	currentTokens := 0

	flush := func() {
		if len(current) == 0 {
			return
		}
		text := formatChunk(current)
		chunks = append(chunks, Chunk{
			SessionPath: sess.Ref.Path,
			ChunkIndex:  len(chunks),
			Hash:        chunkHash(text),
			Text:        text,
			MsgCount:    len(current),
		})
		current = nil
		currentTokens = 0
	}

	for _, msg := range sess.Messages {
		msgTokens := estimateTokens(msg.Text)
		// Flush before appending if this message would push us over budget,
		// unless current is empty (single oversized message goes alone).
		if currentTokens+msgTokens > maxTokens && len(current) > 0 {
			flush()
		}
		current = append(current, msg)
		currentTokens += msgTokens
	}
	flush()

	return chunks
}

// formatChunk renders a slice of messages into the text fed to the LLM.
// Format: "User: <text>\n\nAssistant: <text>\n\n…"
func formatChunk(msgs []ConvMessage) string {
	var sb strings.Builder
	for _, m := range msgs {
		role := "User"
		if m.Role == "assistant" {
			role = "Assistant"
		}
		fmt.Fprintf(&sb, "%s: %s\n\n", role, m.Text)
	}
	return strings.TrimSpace(sb.String())
}

// chunkHash returns the lowercase sha256 hex of the normalized chunk text.
// Used as the dedup key in processed_chunks.
func chunkHash(text string) string {
	h := sha256.Sum256([]byte(strings.ToLower(strings.TrimSpace(text))))
	return hex.EncodeToString(h[:])
}
