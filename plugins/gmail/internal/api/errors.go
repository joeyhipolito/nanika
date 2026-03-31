package api

import (
	"errors"
	"fmt"

	"google.golang.org/api/googleapi"
)

// GmailError represents a structured error from the Gmail API, carrying
// the HTTP status code alongside the message. Callers can use the Is*()
// methods to branch on error category without parsing error strings.
type GmailError struct {
	Message    string
	StatusCode int
	Detail     string
}

// Error implements the error interface.
func (e *GmailError) Error() string {
	if e.Detail != "" {
		return fmt.Sprintf("%s (HTTP %d: %s)", e.Message, e.StatusCode, e.Detail)
	}
	return fmt.Sprintf("%s (HTTP %d)", e.Message, e.StatusCode)
}

// IsAuthError returns true if this is an authentication or authorization error
// (HTTP 401 Unauthorized or 403 Forbidden).
func (e *GmailError) IsAuthError() bool {
	return e.StatusCode == 401 || e.StatusCode == 403
}

// IsRateLimitError returns true if this is a rate-limit error (HTTP 429 Too Many Requests).
func (e *GmailError) IsRateLimitError() bool {
	return e.StatusCode == 429
}

// IsRetryable returns true if the error may succeed on a subsequent attempt.
// Retryable statuses are 429 (rate limit) and any 5xx (server error).
func (e *GmailError) IsRetryable() bool {
	return e.StatusCode == 429 || e.StatusCode >= 500
}

// IsAuthError reports whether err wraps a GmailError with an auth-failure status.
// Uses errors.As so it works through error chains.
func IsAuthError(err error) bool {
	var ge *GmailError
	return errors.As(err, &ge) && ge.IsAuthError()
}

// IsRateLimitError reports whether err wraps a GmailError with a rate-limit status.
// Uses errors.As so it works through error chains.
func IsRateLimitError(err error) bool {
	var ge *GmailError
	return errors.As(err, &ge) && ge.IsRateLimitError()
}

// IsRetryable reports whether err wraps a GmailError that may succeed on retry.
// Uses errors.As so it works through error chains.
func IsRetryable(err error) bool {
	var ge *GmailError
	return errors.As(err, &ge) && ge.IsRetryable()
}

// wrapAPIError converts a *googleapi.Error into a *GmailError, preserving the
// status code and detail message. Non-googleapi errors are returned unchanged.
func wrapAPIError(err error) error {
	if err == nil {
		return nil
	}
	var gErr *googleapi.Error
	if errors.As(err, &gErr) {
		return &GmailError{
			Message:    gErr.Error(),
			StatusCode: gErr.Code,
			Detail:     gErr.Message,
		}
	}
	return err
}
