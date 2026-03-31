package cmd

import "strings"

// normalizeActivityURN accepts either a full activity URN or just a numeric ID,
// and returns the full URN format.
// Examples:
//
//	"urn:li:activity:1234567890" -> "urn:li:activity:1234567890"
//	"1234567890"                 -> "urn:li:activity:1234567890"
//	"urn:li:ugcPost:1234567890"  -> "urn:li:ugcPost:1234567890"
func normalizeActivityURN(input string) string {
	input = strings.TrimSpace(input)
	if strings.HasPrefix(input, "urn:li:") {
		return input
	}
	return "urn:li:activity:" + input
}
