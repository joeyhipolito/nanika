package api

import (
	"encoding/base64"
	"strings"
	"testing"
)

// decodeMIME is a test helper that base64url-decodes the output of buildMIME.
func decodeMIME(t *testing.T, raw string) string {
	t.Helper()
	decoded, err := base64.URLEncoding.DecodeString(raw)
	if err != nil {
		t.Fatalf("base64 decode failed: %v", err)
	}
	return string(decoded)
}

// TestBuildMIME_MissingTo verifies that an empty To address returns an error.
func TestBuildMIME_MissingTo(t *testing.T) {
	_, err := buildMIME(ComposeParams{Subject: "Hi", Body: "Hello"}, "", "", "")
	if err == nil {
		t.Error("expected error for missing To, got nil")
	}
}

// TestBuildMIME_PlainText verifies that a plain-text message has the expected headers and body.
func TestBuildMIME_PlainText(t *testing.T) {
	p := ComposeParams{
		To:      "recipient@example.com",
		Subject: "Test Subject",
		Body:    "Hello, this is a test.",
	}
	raw, err := buildMIME(p, "", "", "")
	if err != nil {
		t.Fatalf("buildMIME returned error: %v", err)
	}

	mime := decodeMIME(t, raw)

	cases := []struct {
		label string
		want  string
	}{
		{"To header", "To: recipient@example.com"},
		{"Subject header", "Subject: Test Subject"},
		{"MIME-Version", "MIME-Version: 1.0"},
		{"Content-Type plain", "Content-Type: text/plain; charset=utf-8"},
		{"body text", "Hello, this is a test."},
	}

	for _, c := range cases {
		if !strings.Contains(mime, c.want) {
			t.Errorf("[%s] MIME missing %q\nfull MIME:\n%s", c.label, c.want, mime)
		}
	}

	// HTML boundary should NOT be present.
	if strings.Contains(mime, "multipart/alternative") {
		t.Error("plain-text message should not contain multipart/alternative")
	}
}

// TestBuildMIME_WithCC verifies that CC and BCC headers are included when set.
func TestBuildMIME_WithCC(t *testing.T) {
	p := ComposeParams{
		To:      "to@example.com",
		CC:      "cc@example.com",
		BCC:     "bcc@example.com",
		Subject: "CC Test",
		Body:    "body",
	}
	raw, err := buildMIME(p, "", "", "")
	if err != nil {
		t.Fatalf("buildMIME returned error: %v", err)
	}

	mime := decodeMIME(t, raw)

	if !strings.Contains(mime, "Cc: cc@example.com") {
		t.Errorf("MIME missing Cc header\nfull MIME:\n%s", mime)
	}
	if !strings.Contains(mime, "Bcc: bcc@example.com") {
		t.Errorf("MIME missing Bcc header\nfull MIME:\n%s", mime)
	}
}

// TestBuildMIME_Multipart verifies that when HTML is provided the message is
// multipart/alternative with both a plain-text and HTML part.
func TestBuildMIME_Multipart(t *testing.T) {
	p := ComposeParams{
		To:      "recipient@example.com",
		Subject: "HTML Test",
		Body:    "Plain text version.",
		HTML:    "<p>HTML version.</p>",
	}
	raw, err := buildMIME(p, "", "", "")
	if err != nil {
		t.Fatalf("buildMIME returned error: %v", err)
	}

	mime := decodeMIME(t, raw)

	if !strings.Contains(mime, "multipart/alternative") {
		t.Error("MIME missing multipart/alternative content type")
	}
	if !strings.Contains(mime, "text/plain") {
		t.Error("MIME missing text/plain part")
	}
	if !strings.Contains(mime, "text/html") {
		t.Error("MIME missing text/html part")
	}
	if !strings.Contains(mime, "Plain text version.") {
		t.Error("MIME missing plain text body")
	}
	if !strings.Contains(mime, "<p>HTML version.</p>") {
		t.Error("MIME missing HTML body")
	}
}

// TestBuildMIME_ReplyHeaders verifies that In-Reply-To and References headers
// are included when provided (i.e., for reply messages).
func TestBuildMIME_ReplyHeaders(t *testing.T) {
	inReplyTo := "<abc123@mail.example.com>"
	refs := "<prev@mail.example.com> <abc123@mail.example.com>"

	p := ComposeParams{
		To:      "original@example.com",
		Subject: "Re: Original Subject",
		Body:    "Reply body.",
	}
	raw, err := buildMIME(p, "threadID", inReplyTo, refs)
	if err != nil {
		t.Fatalf("buildMIME returned error: %v", err)
	}

	mime := decodeMIME(t, raw)

	if !strings.Contains(mime, "In-Reply-To: "+inReplyTo) {
		t.Errorf("MIME missing In-Reply-To header\nfull MIME:\n%s", mime)
	}
	if !strings.Contains(mime, "References: "+refs) {
		t.Errorf("MIME missing References header\nfull MIME:\n%s", mime)
	}
}

// TestBuildMIME_NoReplyHeaders verifies that In-Reply-To and References are
// omitted when not provided (i.e., for new messages).
func TestBuildMIME_NoReplyHeaders(t *testing.T) {
	p := ComposeParams{
		To:      "someone@example.com",
		Subject: "New message",
		Body:    "body",
	}
	raw, err := buildMIME(p, "", "", "")
	if err != nil {
		t.Fatalf("buildMIME returned error: %v", err)
	}

	mime := decodeMIME(t, raw)

	if strings.Contains(mime, "In-Reply-To") {
		t.Error("new message should not have In-Reply-To header")
	}
	if strings.Contains(mime, "References") {
		t.Error("new message should not have References header")
	}
}

// TestHeaderVal_CaseInsensitive verifies that headerVal is case-insensitive.
func TestHeaderVal_CaseInsensitive(t *testing.T) {
	headers := map[string]string{
		"Message-ID": "<msg123@example.com>",
		"References": "<ref456@example.com>",
	}

	got := headerVal(headers, "message-id")
	if got != "<msg123@example.com>" {
		t.Errorf("headerVal(message-id) = %q, want %q", got, "<msg123@example.com>")
	}

	got = headerVal(headers, "REFERENCES")
	if got != "<ref456@example.com>" {
		t.Errorf("headerVal(REFERENCES) = %q, want %q", got, "<ref456@example.com>")
	}
}

// TestHeaderVal_Missing verifies that headerVal returns empty string for absent keys.
func TestHeaderVal_Missing(t *testing.T) {
	headers := map[string]string{"Subject": "hi"}
	got := headerVal(headers, "X-Custom")
	if got != "" {
		t.Errorf("headerVal(missing) = %q, want empty", got)
	}
}
