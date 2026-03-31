// Package transform provides utilities for converting between YNAB's data formats
// and human-readable values.
//
// YNAB uses "milliunits" (1/1000 of currency unit) for all monetary amounts:
//   - $1.00 = 1000 milliunits
//   - $100.00 = 100000 milliunits
//   - $0.01 = 10 milliunits
package transform

import (
	"fmt"
	"math"
	"strconv"
	"strings"
	"time"
)

// DollarsToMilliunits converts a dollar amount to YNAB milliunits.
//
// YNAB uses milliunits (1/1000 of currency unit) for all amounts.
// This ensures precision without floating point errors.
//
// Examples:
//
//	DollarsToMilliunits(100.00)  // 100000
//	DollarsToMilliunits(0.01)    // 10
//	DollarsToMilliunits(1.505)   // 1505 (rounds to nearest milliunit)
//	DollarsToMilliunits(-50.00)  // -50000 (negative for expenses)
func DollarsToMilliunits(dollars float64) int64 {
	// Multiply by 1000 and round to nearest integer
	// Using math.Round for banker's rounding (round half to even)
	milliunits := math.Round(dollars * 1000)
	return int64(milliunits)
}

// MilliunitsToDollars converts YNAB milliunits to a dollar amount.
//
// Examples:
//
//	MilliunitsToDollars(100000)  // 100.0
//	MilliunitsToDollars(10)      // 0.01
//	MilliunitsToDollars(1505)    // 1.505
//	MilliunitsToDollars(-50000)  // -50.0
func MilliunitsToDollars(milliunits int64) float64 {
	// Divide by 1000 to get dollars
	return float64(milliunits) / 1000.0
}

// FormatCurrency formats milliunits as a human-readable currency string.
//
// The function uses "$" as the currency symbol, 2 decimal places,
// and comma as the thousands separator by default.
//
// Examples:
//
//	FormatCurrency(100000)   // "$100.00"
//	FormatCurrency(1505)     // "$1.51"
//	FormatCurrency(-50000)   // "-$50.00"
//	FormatCurrency(1234567)  // "$1,234.57"
func FormatCurrency(milliunits int64) string {
	// Convert to dollars
	dollars := MilliunitsToDollars(milliunits)

	// Handle negative values
	isNegative := dollars < 0
	absDollars := math.Abs(dollars)

	// Format with 2 decimal places
	formatted := formatWithThousands(absDollars, 2)

	// Add currency symbol
	if isNegative {
		return "-$" + formatted
	}
	return "$" + formatted
}

// formatWithThousands formats a float with the specified decimal places
// and adds comma separators for thousands.
func formatWithThousands(value float64, decimals int) string {
	// Format with specified decimal places
	formatted := strconv.FormatFloat(value, 'f', decimals, 64)

	// Split into integer and decimal parts
	parts := strings.Split(formatted, ".")
	intPart := parts[0]
	decPart := ""
	if len(parts) > 1 {
		decPart = parts[1]
	}

	// Add thousands separators to integer part
	intPartWithCommas := addThousandsSeparators(intPart)

	// Combine parts
	if decPart != "" {
		return intPartWithCommas + "." + decPart
	}
	return intPartWithCommas
}

// addThousandsSeparators adds comma separators to a number string.
func addThousandsSeparators(s string) string {
	// Start from the right and insert commas every 3 digits
	n := len(s)
	if n <= 3 {
		return s
	}

	var result strings.Builder
	for i, digit := range s {
		if i > 0 && (n-i)%3 == 0 {
			result.WriteRune(',')
		}
		result.WriteRune(digit)
	}
	return result.String()
}

// ParseMonth parses a month string in YNAB format (YYYY-MM-DD or YYYY-MM)
// and returns the year and month.
//
// Examples:
//
//	ParseMonth("2024-01")     // 2024, 1, nil
//	ParseMonth("2024-01-15")  // 2024, 1, nil (day is ignored)
//	ParseMonth("2024-12")     // 2024, 12, nil
//	ParseMonth("invalid")     // 0, 0, error
func ParseMonth(s string) (year, month int, err error) {
	// Handle both YYYY-MM and YYYY-MM-DD formats
	parts := strings.Split(s, "-")
	if len(parts) < 2 {
		return 0, 0, fmt.Errorf("invalid month format: %s (expected YYYY-MM or YYYY-MM-DD)", s)
	}

	// Parse year
	year, err = strconv.Atoi(parts[0])
	if err != nil {
		return 0, 0, fmt.Errorf("invalid year: %s", parts[0])
	}

	// Parse month
	month, err = strconv.Atoi(parts[1])
	if err != nil {
		return 0, 0, fmt.Errorf("invalid month: %s", parts[1])
	}

	// Validate month range
	if month < 1 || month > 12 {
		return 0, 0, fmt.Errorf("month out of range: %d (must be 1-12)", month)
	}

	return year, month, nil
}

// FormatMonth formats a year and month as YNAB's month string (YYYY-MM).
//
// Examples:
//
//	FormatMonth(2024, 1)   // "2024-01"
//	FormatMonth(2024, 12)  // "2024-12"
func FormatMonth(year, month int) string {
	return fmt.Sprintf("%04d-%02d", year, month)
}

// ParseDate parses a date string in YNAB format (YYYY-MM-DD)
// and returns a time.Time value.
//
// YNAB uses ISO 8601 date format (YYYY-MM-DD) for all dates.
// Times are normalized to UTC midnight.
//
// Examples:
//
//	ParseDate("2024-01-15")  // Jan 15, 2024 00:00:00 UTC
//	ParseDate("2024-12-31")  // Dec 31, 2024 00:00:00 UTC
//	ParseDate("invalid")     // zero time value (use .IsZero() to check)
func ParseDate(s string) time.Time {
	// YNAB uses ISO 8601 format: YYYY-MM-DD
	t, err := time.Parse("2006-01-02", s)
	if err != nil {
		// Return zero time on error
		return time.Time{}
	}
	return t
}

// FormatDate formats a time.Time value as YNAB's date format (YYYY-MM-DD).
//
// Examples:
//
//	FormatDate(time.Date(2024, 1, 15, 0, 0, 0, 0, time.UTC))  // "2024-01-15"
//	FormatDate(time.Now())                                     // "2024-12-31" (example)
func FormatDate(t time.Time) string {
	return t.Format("2006-01-02")
}
