package api

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"google.golang.org/api/drive/v3"
	"google.golang.org/api/option"
)

// newTestDriveClient creates a Client backed by a test HTTP server.
// The provided handler serves all Drive API requests.
func newTestDriveClient(t *testing.T, handler http.Handler) *Client {
	t.Helper()
	srv := httptest.NewServer(handler)
	t.Cleanup(srv.Close)

	drivesvc, err := drive.NewService(context.Background(),
		option.WithoutAuthentication(),
		option.WithEndpoint(srv.URL+"/"),
	)
	if err != nil {
		t.Fatalf("create test drive service: %v", err)
	}
	return &Client{drivesvc: drivesvc, alias: "test"}
}

// fileListResponse builds a drive#fileList JSON response.
func fileListResponse(files []map[string]interface{}) map[string]interface{} {
	return map[string]interface{}{
		"kind":  "drive#fileList",
		"files": files,
	}
}

func TestListDriveFiles_empty(t *testing.T) {
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(fileListResponse(nil))
	})
	c := newTestDriveClient(t, handler)

	files, err := c.ListDriveFiles(10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(files) != 0 {
		t.Errorf("got %d files; want 0", len(files))
	}
}

func TestListDriveFiles_multipleFiles(t *testing.T) {
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(fileListResponse([]map[string]interface{}{
			{
				"id":           "file-1",
				"name":         "report.pdf",
				"mimeType":     "application/pdf",
				"size":         "12345",
				"modifiedTime": "2026-03-19T10:00:00Z",
				"webViewLink":  "https://drive.google.com/file/d/file-1/view",
				"owners":       []map[string]string{{"emailAddress": "user@example.com"}},
			},
			{
				"id":           "file-2",
				"name":         "notes.txt",
				"mimeType":     "text/plain",
				"size":         "512",
				"modifiedTime": "2026-03-18T08:00:00Z",
			},
		}))
	})
	c := newTestDriveClient(t, handler)

	files, err := c.ListDriveFiles(10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(files) != 2 {
		t.Fatalf("got %d files; want 2", len(files))
	}

	f1 := files[0]
	if f1.ID != "file-1" {
		t.Errorf("files[0].ID = %q; want %q", f1.ID, "file-1")
	}
	if f1.Name != "report.pdf" {
		t.Errorf("files[0].Name = %q; want %q", f1.Name, "report.pdf")
	}
	if f1.Account != "test" {
		t.Errorf("files[0].Account = %q; want %q", f1.Account, "test")
	}
	if f1.WebViewLink == "" {
		t.Error("files[0].WebViewLink is empty; want non-empty")
	}
	if len(f1.Owners) != 1 || f1.Owners[0] != "user@example.com" {
		t.Errorf("files[0].Owners = %v; want [user@example.com]", f1.Owners)
	}

	f2 := files[1]
	if f2.ID != "file-2" {
		t.Errorf("files[1].ID = %q; want %q", f2.ID, "file-2")
	}
	if len(f2.Owners) != 0 {
		t.Errorf("files[1].Owners should be empty; got %v", f2.Owners)
	}
}

func TestListDriveFiles_defaultLimit(t *testing.T) {
	var gotPageSize string
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotPageSize = r.URL.Query().Get("pageSize")
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(fileListResponse(nil))
	})
	c := newTestDriveClient(t, handler)

	// limit=0 should default to 10
	if _, err := c.ListDriveFiles(0); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if gotPageSize != "10" {
		t.Errorf("pageSize = %q; want %q", gotPageSize, "10")
	}
}

func TestSearchDriveFiles_emptyQuery(t *testing.T) {
	c := &Client{alias: "test"}
	_, err := c.SearchDriveFiles("", 10)
	if err == nil {
		t.Error("expected error for empty query; got nil")
	}
}

func TestSearchDriveFiles_sendsQuery(t *testing.T) {
	var gotQ string
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotQ = r.URL.Query().Get("q")
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(fileListResponse([]map[string]interface{}{
			{
				"id":           "found-1",
				"name":         "budget 2026.xlsx",
				"mimeType":     "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
				"modifiedTime": "2026-03-01T12:00:00Z",
			},
		}))
	})
	c := newTestDriveClient(t, handler)

	files, err := c.SearchDriveFiles("budget", 5)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(files) != 1 {
		t.Fatalf("got %d files; want 1", len(files))
	}
	if files[0].ID != "found-1" {
		t.Errorf("files[0].ID = %q; want %q", files[0].ID, "found-1")
	}
	if !strings.Contains(gotQ, "budget") {
		t.Errorf("query %q does not contain search term", gotQ)
	}
	if !strings.Contains(gotQ, "trashed = false") {
		t.Errorf("query %q should exclude trashed files", gotQ)
	}
}

func TestDownloadDriveFile_emptyID(t *testing.T) {
	c := &Client{alias: "test"}
	_, err := c.DownloadDriveFile("", "")
	if err == nil {
		t.Error("expected error for empty file ID; got nil")
	}
}

func TestDownloadDriveFile_regularFile(t *testing.T) {
	const fileContent = "hello from drive"
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Query().Get("alt") == "media" {
			// Download request
			w.Header().Set("Content-Type", "application/octet-stream")
			w.Write([]byte(fileContent))
			return
		}
		// Metadata request
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(map[string]interface{}{
			"id":       "file-abc",
			"name":     "hello.txt",
			"mimeType": "text/plain",
		})
	})
	c := newTestDriveClient(t, handler)

	outPath := filepath.Join(t.TempDir(), "hello.txt")
	saved, err := c.DownloadDriveFile("file-abc", outPath)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if saved != outPath {
		t.Errorf("saved = %q; want %q", saved, outPath)
	}
	got, err := os.ReadFile(outPath)
	if err != nil {
		t.Fatalf("read output file: %v", err)
	}
	if string(got) != fileContent {
		t.Errorf("file content = %q; want %q", string(got), fileContent)
	}
}

func TestDownloadDriveFile_googleDoc(t *testing.T) {
	const exportedContent = "exported docx content"
	handler := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.HasSuffix(r.URL.Path, "/export") {
			// Export request
			w.Header().Set("Content-Type", "application/vnd.openxmlformats-officedocument.wordprocessingml.document")
			w.Write([]byte(exportedContent))
			return
		}
		// Metadata request
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(map[string]interface{}{
			"id":       "doc-xyz",
			"name":     "My Doc",
			"mimeType": "application/vnd.google-apps.document",
		})
	})
	c := newTestDriveClient(t, handler)

	outPath := filepath.Join(t.TempDir(), "My Doc.docx")
	saved, err := c.DownloadDriveFile("doc-xyz", outPath)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if saved != outPath {
		t.Errorf("saved = %q; want %q", saved, outPath)
	}
	got, err := os.ReadFile(outPath)
	if err != nil {
		t.Fatalf("read output file: %v", err)
	}
	if string(got) != exportedContent {
		t.Errorf("file content = %q; want %q", string(got), exportedContent)
	}
}

func TestGoogleExportFormat(t *testing.T) {
	tests := []struct {
		mimeType string
		wantMIME string
		wantExt  string
	}{
		{
			"application/vnd.google-apps.document",
			"application/vnd.openxmlformats-officedocument.wordprocessingml.document",
			".docx",
		},
		{
			"application/vnd.google-apps.spreadsheet",
			"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
			".xlsx",
		},
		{
			"application/vnd.google-apps.presentation",
			"application/vnd.openxmlformats-officedocument.presentationml.presentation",
			".pptx",
		},
		{"application/pdf", "", ""},
		{"text/plain", "", ""},
	}
	for _, tt := range tests {
		t.Run(tt.mimeType, func(t *testing.T) {
			gotMIME, gotExt := googleExportFormat(tt.mimeType)
			if gotMIME != tt.wantMIME {
				t.Errorf("exportMIME = %q; want %q", gotMIME, tt.wantMIME)
			}
			if gotExt != tt.wantExt {
				t.Errorf("ext = %q; want %q", gotExt, tt.wantExt)
			}
		})
	}
}

func TestToDriveFile(t *testing.T) {
	f := &drive.File{
		Id:           "f1",
		Name:         "test.pdf",
		MimeType:     "application/pdf",
		Size:         4096,
		ModifiedTime: "2026-03-19T10:00:00Z",
		WebViewLink:  "https://drive.google.com/file/d/f1/view",
		Owners:       []*drive.User{{EmailAddress: "alice@example.com"}},
	}
	df := toDriveFile("work", f)
	if df.ID != "f1" {
		t.Errorf("ID = %q; want %q", df.ID, "f1")
	}
	if df.Account != "work" {
		t.Errorf("Account = %q; want %q", df.Account, "work")
	}
	if df.Name != "test.pdf" {
		t.Errorf("Name = %q; want %q", df.Name, "test.pdf")
	}
	if df.Size != 4096 {
		t.Errorf("Size = %d; want %d", df.Size, 4096)
	}
	if df.ModifiedTime != "2026-03-19T10:00:00Z" {
		t.Errorf("ModifiedTime = %q; want %q", df.ModifiedTime, "2026-03-19T10:00:00Z")
	}
	if len(df.Owners) != 1 || df.Owners[0] != "alice@example.com" {
		t.Errorf("Owners = %v; want [alice@example.com]", df.Owners)
	}
}
