package api

import (
	"errors"
	"time"

	"google.golang.org/api/googleapi"
)

const (
	maxRetries    = 3
	retryBaseWait = time.Second
)

// withRetry executes fn up to maxRetries+1 times. On a transient failure
// (HTTP 429 Too Many Requests or any 5xx Server Error) it waits with
// exponential backoff (1s, 2s, 4s) before each retry.
//
// Auth errors (401, 403) and client errors (4xx except 429) are not retried.
//
// The final error is wrapped as a *GmailError when the underlying cause is
// a *googleapi.Error, so callers can use IsAuthError/IsRateLimitError/IsRetryable.
func withRetry(fn func() error) error {
	var lastErr error
	for attempt := 0; attempt <= maxRetries; attempt++ {
		if attempt > 0 {
			wait := retryBaseWait * (1 << uint(attempt-1))
			time.Sleep(wait)
		}

		err := fn()
		if err == nil {
			return nil
		}
		lastErr = err

		if !isTransient(err) {
			break
		}
	}
	return wrapAPIError(lastErr)
}

// isTransient returns true when err is a Google API error with a status code
// that warrants a retry: 429 (rate limit) or any 5xx (server error).
func isTransient(err error) bool {
	var gErr *googleapi.Error
	if errors.As(err, &gErr) {
		return gErr.Code == 429 || gErr.Code >= 500
	}
	return false
}
