package dream

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
	"github.com/joeyhipolito/orchestrator-cli/internal/sanitize"
)

// Runner orchestrates one dream extraction cycle:
// discover → hash → parse → filter → chunk → extract → store.
type Runner struct {
	store      Store
	learningDB *learning.DB
	embedder   *learning.Embedder
	cfg        Config
}

// NewRunner creates a Runner with the given components.
func NewRunner(store Store, ldb *learning.DB, embedder *learning.Embedder, cfg Config) *Runner {
	return &Runner{
		store:      store,
		learningDB: ldb,
		embedder:   embedder,
		cfg:        cfg,
	}
}

// Run executes one dream cycle and returns a Report describing what happened.
// domain is passed through to extracted learnings. workspaceID may be empty.
func (r *Runner) Run(ctx context.Context, domain, workspaceID string) (Report, error) {
	report := Report{StartedAt: time.Now()}

	refs, err := r.discover()
	if err != nil {
		return report, fmt.Errorf("discover transcripts: %w", err)
	}
	report.Discovered = len(refs)

	for _, ref := range refs {
		if ctx.Err() != nil {
			break
		}
		if err := r.processFile(ctx, ref, domain, workspaceID, &report); err != nil {
			report.Errors = append(report.Errors, RunError{
				Path:  ref.Path,
				Phase: "process",
				Err:   err.Error(),
			})
		}
	}

	report.Duration = time.Since(report.StartedAt)
	return report, nil
}

// discover walks the configured root directory and returns candidate
// transcript refs, sorted newest-first and bounded by MaxFilesPerRun/Limit.
func (r *Runner) discover() ([]TranscriptRef, error) {
	root := r.cfg.RootDir
	if root == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return nil, fmt.Errorf("user home dir: %w", err)
		}
		root = filepath.Join(home, ".claude", "projects")
	}

	if _, err := os.Stat(root); os.IsNotExist(err) {
		return nil, nil // nothing to process if the directory doesn't exist yet
	}

	now := time.Now()
	var refs []TranscriptRef

	err := filepath.WalkDir(root, func(path string, d os.DirEntry, walkErr error) error {
		if walkErr != nil {
			return nil // skip unreadable directories
		}
		if d.IsDir() {
			return nil
		}
		if !strings.HasSuffix(path, ".jsonl") {
			return nil
		}

		info, err := d.Info()
		if err != nil {
			return nil
		}

		// Age filter: skip transcripts older than MaxTranscriptAge.
		if r.cfg.MaxTranscriptAge > 0 && now.Sub(info.ModTime()) > r.cfg.MaxTranscriptAge {
			return nil
		}

		// Since filter: skip transcripts modified before the cutoff.
		if !r.cfg.Since.IsZero() && info.ModTime().Before(r.cfg.Since) {
			return nil
		}

		// Session filter: substring match on path.
		if r.cfg.SessionFilter != "" && !strings.Contains(path, r.cfg.SessionFilter) {
			return nil
		}

		refs = append(refs, TranscriptRef{
			Path:    path,
			ModTime: info.ModTime(),
		})
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("walking %s: %w", root, err)
	}

	// Sort newest-first so MaxFilesPerRun picks the most recent transcripts.
	sort.Slice(refs, func(i, j int) bool {
		return refs[i].ModTime.After(refs[j].ModTime)
	})

	limit := r.cfg.MaxFilesPerRun
	if r.cfg.Limit > 0 && r.cfg.Limit < limit {
		limit = r.cfg.Limit
	}
	if limit > 0 && len(refs) > limit {
		refs = refs[:limit]
	}

	return refs, nil
}

// processFile runs the full pipeline for one transcript file.
func (r *Runner) processFile(ctx context.Context, ref TranscriptRef, domain, workspaceID string, report *Report) error {
	// Layer 1 dedup: file-level content hash.
	hash, err := hashFile(ref.Path)
	if err != nil {
		report.SkippedFile++
		return fmt.Errorf("hashing %s: %w", ref.Path, err)
	}
	ref.ContentHash = hash

	if !r.cfg.Force {
		processed, err := r.store.IsFileProcessed(hash)
		if err != nil {
			return fmt.Errorf("checking file processed: %w", err)
		}
		if processed {
			report.SkippedFile++
			if r.cfg.Verbose {
				fmt.Printf("dream: skip %s (unchanged)\n", filepath.Base(ref.Path))
			}
			return nil
		}
	}

	// Parse JSONL transcript.
	msgs, cwd, err := ParseTranscript(ref.Path)
	if err != nil {
		report.SkippedFile++
		report.Errors = append(report.Errors, RunError{Path: ref.Path, Phase: "parse", Err: err.Error()})
		return nil // non-fatal: skip the file
	}

	// Filter: skip worker sessions to avoid re-mining live-captured content.
	if isWorkerSession(cwd) {
		report.SkippedFile++
		if r.cfg.Verbose {
			fmt.Printf("dream: skip %s (worker session)\n", filepath.Base(ref.Path))
		}
		return nil
	}

	// Filter: skip sessions too short to contain useful signal.
	if len(msgs) < r.cfg.MinSessionMsgs {
		report.SkippedFile++
		return nil
	}

	sess := Session{Ref: ref, Cwd: cwd, Messages: msgs}
	chunks := ChunkSession(sess, r.cfg.MaxChunkTokens)
	report.ProcessedFiles++
	report.ChunksEmitted += len(chunks)

	if r.cfg.Verbose {
		fmt.Printf("dream: %s — %d msgs → %d chunks\n",
			filepath.Base(ref.Path), len(msgs), len(chunks))
	}

	if r.cfg.DryRun {
		return nil
	}

	storedTotal := 0
	for _, chunk := range chunks {
		if ctx.Err() != nil {
			break
		}
		stored, skipped, err := r.processChunk(ctx, chunk, domain, workspaceID, report)
		if err != nil {
			report.Errors = append(report.Errors, RunError{Path: ref.Path, Phase: "extract", Err: err.Error()})
		}
		if skipped {
			report.ChunksSkipped++
		}
		storedTotal += stored
	}
	report.LearningsStored += storedTotal

	// Mark file processed after all chunks succeed (or partially succeed).
	if err := r.store.MarkFileProcessed(ref.Path, hash, len(msgs), len(chunks)); err != nil {
		return fmt.Errorf("marking file processed: %w", err)
	}
	return nil
}

// processChunk runs Layer 2 dedup (chunk hash), calls the LLM extractor,
// and persists results. Returns (stored, wasSkipped, error).
func (r *Runner) processChunk(ctx context.Context, chunk Chunk, domain, workspaceID string, report *Report) (stored int, skipped bool, err error) {
	// Layer 2 dedup: chunk-level hash.
	isProcessed, err := r.store.IsChunkProcessed(chunk.Hash)
	if err != nil {
		return 0, false, fmt.Errorf("checking chunk %s: %w", chunk.Hash[:8], err)
	}
	if isProcessed {
		return 0, true, nil
	}

	// Sanitize before sending to LLM — transcripts may contain invisible chars
	// or bidi overrides from pasted user content.
	safeText := sanitize.SanitizeText(chunk.Text)

	// Call LLM.
	report.LLMCalls++
	learnings := learning.CaptureFromConversation(ctx, safeText, "dream", domain, workspaceID)

	// Tag each learning with dream provenance and persist.
	chunkCtx := "dream:" + chunk.Hash[:8]
	for i := range learnings {
		learnings[i].Context = chunkCtx
		if insertErr := r.learningDB.Insert(ctx, learnings[i], r.embedder); insertErr != nil {
			report.LearningsRejected++
			continue
		}
		stored++
	}
	// Count learnings returned by LLM but blocked by Insert dedup.
	report.LearningsRejected += len(learnings) - stored

	// Mark chunk processed regardless of how many learnings stuck.
	if markErr := r.store.MarkChunkProcessed(chunk.SessionPath, chunk.Hash, chunk.ChunkIndex); markErr != nil {
		return stored, false, fmt.Errorf("marking chunk processed: %w", markErr)
	}
	return stored, false, nil
}

// hashFile computes the sha256 hex digest of the file at path.
func hashFile(path string) (string, error) {
	f, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer f.Close()

	h := sha256.New()
	if _, err := io.Copy(h, f); err != nil {
		return "", err
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}
