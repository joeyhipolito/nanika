// Package transform provides utilities for formatting and converting monetary values.
package transform

import (
	"fmt"
	"math"
	"strconv"
	"strings"
)

// CurrencySymbol returns the display symbol for a currency code.
// NZD and USD use "$", PHP uses "₱". Unknown currencies fall back to "$".
func CurrencySymbol(currency string) string {
	switch currency {
	case "PHP":
		return "₱"
	default:
		return "$"
	}
}

// FormatAmount formats an amount in cents with the correct currency symbol.
//
// Examples:
//
//	FormatAmount(10000, "NZD")  // "$100.00"
//	FormatAmount(10000, "PHP")  // "₱100.00"
//	FormatAmount(-5000, "NZD")  // "-$50.00"
func FormatAmount(cents int64, currency string) string {
	sym := CurrencySymbol(currency)
	dollars := float64(cents) / 100.0
	isNegative := dollars < 0
	absDollars := math.Abs(dollars)
	formatted := formatWithThousands(absDollars, 2)
	if isNegative {
		return "-" + sym + formatted
	}
	return sym + formatted
}

// FormatCurrency formats an amount in cents as a human-readable currency string.
//
// The function uses "$" as the currency symbol, 2 decimal places,
// and comma as the thousands separator.
//
// Examples:
//
//	FormatCurrency(10000)   // "$100.00"
//	FormatCurrency(151)     // "$1.51"
//	FormatCurrency(-5000)   // "-$50.00"
//	FormatCurrency(123456)  // "$1,234.56"
func FormatCurrency(cents int64) string {
	// Convert cents to dollars
	dollars := float64(cents) / 100.0

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

// ParseCents converts a string dollar amount to int64 cents.
// "50" → 5000, "1.25" → 125. Does not handle sign; callers negate for expenses.
func ParseCents(s string) (int64, error) {
	f, err := strconv.ParseFloat(s, 64)
	if err != nil {
		return 0, err
	}
	return int64(math.Round(f * 100)), nil
}

// CentsToPercent formats a decimal amount as a percentage string.
//
// Examples:
//
//	CentsToPercent(50)    // "0.50%"
//	CentsToPercent(1000)  // "10.00%"
//	CentsToPercent(12345) // "123.45%"
func CentsToPercent(cents int64) string {
	percent := float64(cents) / 100.0
	return fmt.Sprintf("%.2f%%", percent)
}
