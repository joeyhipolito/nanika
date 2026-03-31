package api

import (
	"errors"
	"fmt"
	"net/http"
)

// YNABError represents an error from the YNAB API.
type YNABError struct {
	Message    string
	StatusCode int
	ErrorID    string
	Detail     string
}

func (e *YNABError) Error() string {
	msg := fmt.Sprintf("[YNAB] %s", e.Message)
	if e.StatusCode > 0 {
		msg += fmt.Sprintf(" (HTTP %d)", e.StatusCode)
	}
	if e.ErrorID != "" {
		msg += fmt.Sprintf(" [Error ID: %s]", e.ErrorID)
	}
	if e.Detail != "" {
		msg += fmt.Sprintf("\nDetails: %s", e.Detail)
	}
	return msg
}

// IsAuthError returns true if the error is an authentication error (401).
func (e *YNABError) IsAuthError() bool {
	return e.StatusCode == http.StatusUnauthorized
}

// IsRateLimitError returns true if the error is a rate limit error (429).
func (e *YNABError) IsRateLimitError() bool {
	return e.StatusCode == http.StatusTooManyRequests
}

// IsServerError returns true if the error is a server error (5xx).
func (e *YNABError) IsServerError() bool {
	return e.StatusCode >= 500 && e.StatusCode < 600
}

// IsNotFoundError returns true if the error is a not found error (404).
func (e *YNABError) IsNotFoundError() bool {
	return e.StatusCode == http.StatusNotFound
}

// IsBadRequestError returns true if the error is a bad request error (400).
func (e *YNABError) IsBadRequestError() bool {
	return e.StatusCode == http.StatusBadRequest
}

// IsRetryable returns true if the error is potentially retryable.
// This includes rate limit errors, server errors, and network errors.
func (e *YNABError) IsRetryable() bool {
	return e.IsRateLimitError() || e.IsServerError()
}

// Helper functions for error checking

// IsYNABError returns true if the error is a YNABError.
func IsYNABError(err error) bool {
	var ynabErr *YNABError
	return errors.As(err, &ynabErr)
}

// IsAuthError returns true if the error is a YNAB authentication error.
func IsAuthError(err error) bool {
	var ynabErr *YNABError
	if errors.As(err, &ynabErr) {
		return ynabErr.IsAuthError()
	}
	return false
}

// IsRateLimitError returns true if the error is a YNAB rate limit error.
func IsRateLimitError(err error) bool {
	var ynabErr *YNABError
	if errors.As(err, &ynabErr) {
		return ynabErr.IsRateLimitError()
	}
	return false
}

// IsServerError returns true if the error is a YNAB server error.
func IsServerError(err error) bool {
	var ynabErr *YNABError
	if errors.As(err, &ynabErr) {
		return ynabErr.IsServerError()
	}
	return false
}

// IsNotFoundError returns true if the error is a YNAB not found error.
func IsNotFoundError(err error) bool {
	var ynabErr *YNABError
	if errors.As(err, &ynabErr) {
		return ynabErr.IsNotFoundError()
	}
	return false
}

// NewAuthError creates a new authentication error.
func NewAuthError() *YNABError {
	return &YNABError{
		Message:    "Unauthorized: Invalid or missing access token",
		StatusCode: http.StatusUnauthorized,
		Detail:     "Please check your YNAB_ACCESS_TOKEN environment variable",
	}
}

// NewRateLimitError creates a new rate limit error.
func NewRateLimitError(retryAfter int) *YNABError {
	return &YNABError{
		Message:    fmt.Sprintf("Rate limit exceeded. Retry after %d seconds", retryAfter),
		StatusCode: http.StatusTooManyRequests,
		Detail:     "YNAB API has rate limits. Please wait before retrying",
	}
}

// NewServerError creates a new server error.
func NewServerError(statusCode int) *YNABError {
	return &YNABError{
		Message:    "YNAB server error",
		StatusCode: statusCode,
		Detail:     "The YNAB API is experiencing issues. Please try again later",
	}
}
