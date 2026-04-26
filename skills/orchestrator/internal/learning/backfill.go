package learning

import (
	"context"
	"errors"
	"fmt"
	"io"
	"time"
)

// BackfillOptions controls how the embedding backfill walker behaves.
type BackfillOptions struct {
	MaxAge          time.Duration // 0 = no age filter
	IncludeArchived bool
	Limit           int           // 0 = no cap
	BatchSize       int           // rows per batchEmbedContents call (1..100)
	RPM             int           // requests per minute throttle, 0 = unthrottled
	MaxRetries      int           // per-batch retry ceiling on 429 / 5xx
	MaxContentChars int           // truncate content before send (0 = no cap)
	Logf            func(format string, args ...any)
	Out             io.Writer // optional progress sink for per-batch lines (nil = silent)
}

// BackfillResult tallies the outcome of a backfill run.
type BackfillResult struct {
	Processed     int           // candidates pulled from the DB
	Embedded      int           // rows whose embedding column was filled
	AlreadyFilled int           // candidates that another writer beat us to
	Failed        []string      // candidate IDs the API never produced for
	Elapsed       time.Duration // wall-clock time of the run
}

// embedBatchFunc abstracts the embedder so tests can drive the backfill
// without standing up an httptest server. Production callers pass
// embedder.EmbedBatch directly.
type embedBatchFunc func(ctx context.Context, texts []string) ([][]float32, error)

// BackfillEmbeddings walks rows where embedding IS NULL and fills them via
// embed. Defaults: BatchSize=100, RPM=60, MaxRetries=5. Sleeps between batches
// to honor RPM. Honors HTTPStatusError.RetryAfter when present, otherwise
// applies exponential backoff (1s,2s,4s,8s,16s,...) capped at 60s.
func BackfillEmbeddings(ctx context.Context, db *DB, embed embedBatchFunc, opts BackfillOptions) (BackfillResult, error) {
	if db == nil {
		return BackfillResult{}, errors.New("backfill: nil DB")
	}
	if embed == nil {
		return BackfillResult{}, errors.New("backfill: nil embed function")
	}
	if opts.BatchSize <= 0 {
		opts.BatchSize = 100
	}
	if opts.BatchSize > 100 {
		opts.BatchSize = 100
	}
	if opts.MaxRetries < 0 {
		opts.MaxRetries = 0
	}

	candidates, err := db.SelectEmbeddingBackfill(ctx, opts.MaxAge, opts.IncludeArchived, opts.Limit)
	if err != nil {
		return BackfillResult{}, err
	}

	res := BackfillResult{}
	if len(candidates) == 0 {
		return res, nil
	}

	start := time.Now()
	var perBatchSleep time.Duration
	if opts.RPM > 0 {
		perBatchSleep = time.Minute / time.Duration(opts.RPM)
	}
	var batchStart time.Time

	for offset := 0; offset < len(candidates); offset += opts.BatchSize {
		end := offset + opts.BatchSize
		if end > len(candidates) {
			end = len(candidates)
		}
		batch := candidates[offset:end]
		texts := make([]string, len(batch))
		for i, c := range batch {
			texts[i] = truncateForEmbedding(c.Content, opts.MaxContentChars)
		}

		// Throttle: ensure perBatchSleep elapsed since the previous batch start.
		if perBatchSleep > 0 && !batchStart.IsZero() {
			if wait := perBatchSleep - time.Since(batchStart); wait > 0 {
				if err := sleepCtx(ctx, wait); err != nil {
					res.Elapsed = time.Since(start)
					return res, err
				}
			}
		}
		batchStart = time.Now()

		vecs, err := embedBatchWithRetry(ctx, embed, texts, opts.MaxRetries, opts.Logf)
		res.Processed += len(batch)
		if err != nil {
			for _, c := range batch {
				res.Failed = append(res.Failed, c.ID)
			}
			if opts.Logf != nil {
				opts.Logf("backfill: batch starting %s failed after retries: %v", batch[0].ID, err)
			}
			// Continue with the next batch — partial progress is preferable
			// to dropping the rest of the run for a single bad page.
			continue
		}

		for i, c := range batch {
			ok, err := db.SetEmbedding(ctx, c.ID, vecs[i])
			switch {
			case err != nil:
				res.Failed = append(res.Failed, c.ID)
				if opts.Logf != nil {
					opts.Logf("backfill: write failed for %s: %v", c.ID, err)
				}
			case ok:
				res.Embedded++
			default:
				res.AlreadyFilled++
			}
		}
		if opts.Logf != nil {
			opts.Logf("backfill: batch %d-%d processed (embedded=%d already=%d failed=%d)",
				offset+1, end, res.Embedded, res.AlreadyFilled, len(res.Failed))
		}
	}

	res.Elapsed = time.Since(start)
	return res, nil
}

func embedBatchWithRetry(ctx context.Context, embed embedBatchFunc, texts []string, maxRetries int, logf func(string, ...any)) ([][]float32, error) {
	var lastErr error
	for attempt := 0; attempt <= maxRetries; attempt++ {
		if attempt > 0 {
			delay := backoffDelay(attempt)
			var hse *HTTPStatusError
			if errors.As(lastErr, &hse) && hse.RetryAfter > 0 {
				delay = hse.RetryAfter
			}
			if logf != nil {
				logf("backfill: retry %d/%d in %s (last err: %v)", attempt, maxRetries, delay, lastErr)
			}
			if err := sleepCtx(ctx, delay); err != nil {
				return nil, err
			}
		}
		vecs, err := embed(ctx, texts)
		if err == nil {
			return vecs, nil
		}
		lastErr = err
		if !isRetryable(err) {
			return nil, err
		}
	}
	return nil, fmt.Errorf("batch embedding exhausted %d retries: %w", maxRetries, lastErr)
}

func isRetryable(err error) bool {
	var hse *HTTPStatusError
	if errors.As(err, &hse) {
		return hse.Status == 429 || hse.Status >= 500
	}
	// Network / context errors fall through as non-retryable here; the caller
	// treats them as terminal for this batch (the next batch may still work).
	return false
}

func backoffDelay(attempt int) time.Duration {
	const cap = 60 * time.Second
	d := time.Second
	for i := 1; i < attempt; i++ {
		d *= 2
		if d >= cap {
			return cap
		}
	}
	return d
}

func sleepCtx(ctx context.Context, d time.Duration) error {
	if d <= 0 {
		return nil
	}
	t := time.NewTimer(d)
	defer t.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-t.C:
		return nil
	}
}

func truncateForEmbedding(s string, max int) string {
	if max <= 0 || len(s) <= max {
		return s
	}
	return s[:max]
}
