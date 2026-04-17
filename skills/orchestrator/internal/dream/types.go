// Package dream mines Claude Code session transcripts for durable learnings.
// It walks ~/.claude/projects/*.jsonl, chunks conversations into token-bounded
// windows, calls Haiku to extract decisions/insights/patterns/gotchas, and
// writes them into the existing learnings.db alongside live-capture learnings.
package dream

import "time"

// TranscriptRef identifies a discovered JSONL transcript file.
type TranscriptRef struct {
	Path        string
	ModTime     time.Time
	ContentHash string // sha256 hex; populated by Runner before processing
}

// ConvMessage is a single parsed turn from a Claude Code JSONL transcript.
type ConvMessage struct {
	Role   string // "user" or "assistant"
	Text   string // concatenated text content from the message
	SeqNum int    // 1-indexed line number in source file
}

// Session groups the parsed messages from one transcript file.
type Session struct {
	Ref      TranscriptRef
	Cwd      string        // working directory from system/init line
	Messages []ConvMessage
}

// Chunk is a token-bounded window of a Session ready for LLM extraction.
type Chunk struct {
	SessionPath string
	ChunkIndex  int
	Hash        string // sha256 of normalized chunk text
	Text        string // formatted "User: …\n\nAssistant: …" block
	MsgCount    int
}

// Config controls dream pipeline behaviour.
type Config struct {
	// Root directory to scan. Defaults to ~/.claude/projects.
	RootDir string

	// Max approximate tokens per chunk before forcing a split. Default 6000.
	MaxChunkTokens int

	// Skip sessions with fewer than this many messages. Default 4.
	MinSessionMsgs int

	// Maximum learnings the LLM may return per chunk. Default 5.
	MaxLearningsPerChunk int

	// Bound work per run. Default 20. Overridden by Limit when nonzero.
	MaxFilesPerRun int

	// Skip transcripts older than this. Zero = no limit. Default 60 days.
	MaxTranscriptAge time.Duration

	// Reprocess transcripts even if already in processed_transcripts.
	Force bool

	// Dry run: parse and chunk but skip LLM calls and DB writes.
	DryRun bool

	// Only process transcripts whose path contains this string.
	SessionFilter string

	// Only process transcripts modified at or after this time. Zero = no filter.
	Since time.Time

	// Override MaxFilesPerRun when nonzero.
	Limit int

	// Emit per-file progress to stdout.
	Verbose bool
}

// DefaultConfig returns a Config with sensible production defaults.
func DefaultConfig() Config {
	return Config{
		MaxChunkTokens:       6000,
		MinSessionMsgs:       4,
		MaxLearningsPerChunk: 5,
		MaxFilesPerRun:       20,
		MaxTranscriptAge:     60 * 24 * time.Hour,
	}
}

// Report summarises one dream run.
type Report struct {
	StartedAt          time.Time
	Duration           time.Duration
	Discovered         int
	SkippedFile        int // excluded, too old, too short, unchanged, worker session
	ProcessedFiles     int
	ChunksEmitted      int
	ChunksSkipped      int // chunk_hash already in processed_chunks
	LLMCalls           int
	LearningsStored    int // learnings returned by CaptureFromConversation
	LearningsRejected  int // returned but failed Insert dedup
	Errors             []RunError
}

// RunError records an error from a specific pipeline phase.
type RunError struct {
	Path  string
	Phase string // "hash"|"parse"|"chunk"|"extract"|"store"
	Err   string
}
