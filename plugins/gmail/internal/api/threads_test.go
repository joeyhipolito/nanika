package api

import (
	"encoding/base64"
	"testing"

	"google.golang.org/api/gmail/v1"
)

// b64url encodes s with standard URL encoding (padded), as some Gmail messages use.
func b64url(s string) string {
	return base64.URLEncoding.EncodeToString([]byte(s))
}

// b64rawurl encodes s with raw URL encoding (no padding), the most common Gmail variant.
func b64rawurl(s string) string {
	return base64.RawURLEncoding.EncodeToString([]byte(s))
}

// makeBodyPart builds a MessagePart with the given MIME type and base64url body data.
func makeBodyPart(mimeType, data string) *gmail.MessagePart {
	return &gmail.MessagePart{
		MimeType: mimeType,
		Body:     &gmail.MessagePartBody{Data: data},
	}
}

// TestDecodeBody_NilPayload verifies that a nil payload produces an empty string.
func TestDecodeBody_NilPayload(t *testing.T) {
	got := decodeBody(nil)
	if got != "" {
		t.Errorf("decodeBody(nil) = %q, want %q", got, "")
	}
}

// TestDecodeBody_EmptyBody verifies that a payload with no data produces an empty string.
func TestDecodeBody_EmptyBody(t *testing.T) {
	p := &gmail.MessagePart{Body: &gmail.MessagePartBody{Data: ""}}
	got := decodeBody(p)
	if got != "" {
		t.Errorf("decodeBody(empty) = %q, want %q", got, "")
	}
}

// TestDecodeBody_URLEncoding verifies that standard base64url (padded) data is decoded.
func TestDecodeBody_URLEncoding(t *testing.T) {
	want := "Hello, World!"
	p := makeBodyPart("text/plain", b64url(want))
	got := decodeBody(p)
	if got != want {
		t.Errorf("decodeBody(URLEncoding) = %q, want %q", got, want)
	}
}

// TestDecodeBody_RawURLEncoding verifies that unpadded base64url data (Gmail's
// common variant) is decoded correctly via the fallback path.
func TestDecodeBody_RawURLEncoding(t *testing.T) {
	want := "Hello, World!"
	// b64rawurl produces unpadded encoding; URLEncoding.DecodeString will fail on it.
	p := makeBodyPart("text/plain", b64rawurl(want))
	got := decodeBody(p)
	if got != want {
		t.Errorf("decodeBody(RawURLEncoding) = %q, want %q", got, want)
	}
}

// TestDecodeBody_MultipartPrefersPlain verifies that when a multipart payload
// contains both text/plain and text/html parts, the plain text is returned.
func TestDecodeBody_MultipartPrefersPlain(t *testing.T) {
	plainText := "plain body"
	htmlText := "<p>html body</p>"

	p := &gmail.MessagePart{
		MimeType: "multipart/alternative",
		Parts: []*gmail.MessagePart{
			makeBodyPart("text/html", b64rawurl(htmlText)),
			makeBodyPart("text/plain", b64rawurl(plainText)),
		},
	}

	got := decodeBody(p)
	if got != plainText {
		t.Errorf("decodeBody(multipart) = %q, want plain text %q", got, plainText)
	}
}

// TestDecodeBody_MultipartFallsBackToHTML verifies that when only text/html is
// present in a multipart payload, the HTML body is returned.
func TestDecodeBody_MultipartFallsBackToHTML(t *testing.T) {
	htmlText := "<p>html only</p>"

	p := &gmail.MessagePart{
		MimeType: "multipart/alternative",
		Parts: []*gmail.MessagePart{
			makeBodyPart("text/html", b64rawurl(htmlText)),
		},
	}

	got := decodeBody(p)
	if got != htmlText {
		t.Errorf("decodeBody(html-only multipart) = %q, want %q", got, htmlText)
	}
}

// TestDecodeBody_NestedMultipart verifies that text/plain is found when it is
// nested inside a multipart/mixed > multipart/alternative structure.
func TestDecodeBody_NestedMultipart(t *testing.T) {
	plainText := "nested plain"

	inner := &gmail.MessagePart{
		MimeType: "multipart/alternative",
		Parts: []*gmail.MessagePart{
			makeBodyPart("text/plain", b64rawurl(plainText)),
		},
	}
	outer := &gmail.MessagePart{
		MimeType: "multipart/mixed",
		Parts:    []*gmail.MessagePart{inner},
	}

	got := decodeBody(outer)
	if got != plainText {
		t.Errorf("decodeBody(nested) = %q, want %q", got, plainText)
	}
}

// TestExtractHeader_Present verifies that a header is returned when present.
func TestExtractHeader_Present(t *testing.T) {
	msg := &gmail.Message{
		Payload: &gmail.MessagePart{
			Headers: []*gmail.MessagePartHeader{
				{Name: "Subject", Value: "Test Subject"},
				{Name: "From", Value: "sender@example.com"},
			},
		},
	}

	got := extractHeader(msg, "Subject")
	if got != "Test Subject" {
		t.Errorf("extractHeader(Subject) = %q, want %q", got, "Test Subject")
	}
}

// TestExtractHeader_CaseInsensitive verifies that header lookup is case-insensitive.
func TestExtractHeader_CaseInsensitive(t *testing.T) {
	msg := &gmail.Message{
		Payload: &gmail.MessagePart{
			Headers: []*gmail.MessagePartHeader{
				{Name: "SUBJECT", Value: "Case Test"},
			},
		},
	}

	got := extractHeader(msg, "subject")
	if got != "Case Test" {
		t.Errorf("extractHeader(case-insensitive) = %q, want %q", got, "Case Test")
	}
}

// TestExtractHeader_Missing verifies that an empty string is returned for absent headers.
func TestExtractHeader_Missing(t *testing.T) {
	msg := &gmail.Message{
		Payload: &gmail.MessagePart{
			Headers: []*gmail.MessagePartHeader{},
		},
	}

	got := extractHeader(msg, "Subject")
	if got != "" {
		t.Errorf("extractHeader(missing) = %q, want %q", got, "")
	}
}

// TestContainsLabel_Found verifies that containsLabel returns true for a present label.
func TestContainsLabel_Found(t *testing.T) {
	labels := []string{"INBOX", "UNREAD", "Label_1"}
	if !containsLabel(labels, "UNREAD") {
		t.Error("containsLabel(UNREAD) = false, want true")
	}
}

// TestContainsLabel_NotFound verifies that containsLabel returns false for an absent label.
func TestContainsLabel_NotFound(t *testing.T) {
	labels := []string{"INBOX", "Label_1"}
	if containsLabel(labels, "UNREAD") {
		t.Error("containsLabel(UNREAD) = true, want false")
	}
}
