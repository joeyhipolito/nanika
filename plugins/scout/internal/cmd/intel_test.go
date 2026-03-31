package cmd

import (
	"bytes"
	"encoding/csv"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/nanika-scout/internal/gather"
)

func setupTempIntelDir(t *testing.T) (string, func()) {
	tmpDir := t.TempDir()
	intelDir := filepath.Join(tmpDir, "intel")
	if err := os.MkdirAll(intelDir, 0755); err != nil {
		t.Fatalf("failed to create intel dir: %v", err)
	}

	oldConfigDir := os.Getenv("SCOUT_CONFIG_DIR")
	os.Setenv("SCOUT_CONFIG_DIR", tmpDir)

	cleanup := func() {
		os.Setenv("SCOUT_CONFIG_DIR", oldConfigDir)
	}
	return intelDir, cleanup
}

func createTestIntelFiles(t *testing.T, intelDir, topicName string) {
	topicDir := filepath.Join(intelDir, topicName)
	if err := os.MkdirAll(topicDir, 0755); err != nil {
		t.Fatalf("failed to create topic dir: %v", err)
	}

	// Create first intel file
	file1 := gather.IntelFile{
		Topic:     topicName,
		GatheredAt: time.Now().Add(-24 * time.Hour),
		Source:    "rss",
		Items: []gather.IntelItem{
			{
				ID:        "item-1",
				Title:     "First Article",
				Content:   "This is the first article content",
				SourceURL: "https://example.com/article1",
				Author:    "John Doe",
				Timestamp: time.Now().Add(-24 * time.Hour),
				Tags:      []string{"golang", "testing"},
				Score:     85.5,
				Engagement: 42,
			},
			{
				ID:        "item-2",
				Title:     "Second Article",
				Content:   "This is the second article content",
				SourceURL: "https://example.com/article2",
				Author:    "Jane Smith",
				Timestamp: time.Now().Add(-20 * time.Hour),
				Tags:      []string{"golang"},
				Score:     72.0,
				Engagement: 28,
			},
		},
	}

	// Create second intel file
	file2 := gather.IntelFile{
		Topic:     topicName,
		GatheredAt: time.Now(),
		Source:    "github",
		Items: []gather.IntelItem{
			{
				ID:        "item-3",
				Title:     "Third Article",
				Content:   "This is the third article content",
				SourceURL: "https://example.com/article3",
				Author:    "Bob Johnson",
				Timestamp: time.Now(),
				Tags:      []string{"testing", "ci"},
				Score:     95.0,
				Engagement: 156,
			},
		},
	}

	writeIntelTestFile(t, topicDir, "2006-01-02T150405_rss.json", file1)
	writeIntelTestFile(t, topicDir, "2006-01-02T150406_github.json", file2)
}

func writeIntelTestFile(t *testing.T, topicDir, filename string, file gather.IntelFile) {
	filePath := filepath.Join(topicDir, filename)
	data, err := json.MarshalIndent(file, "", "  ")
	if err != nil {
		t.Fatalf("failed to marshal intel file: %v", err)
	}
	if err := os.WriteFile(filePath, data, 0644); err != nil {
		t.Fatalf("failed to write intel file: %v", err)
	}
}

func captureOutput(fn func() error) (string, error) {
	oldStdout := os.Stdout
	r, w, err := os.Pipe()
	if err != nil {
		return "", err
	}
	os.Stdout = w

	err = fn()

	w.Close()
	os.Stdout = oldStdout

	var buf bytes.Buffer
	io.Copy(&buf, r)
	r.Close()

	return buf.String(), err
}

func TestIntelListMarkdownFormat(t *testing.T) {
	intelDir, cleanup := setupTempIntelDir(t)
	defer cleanup()

	createTestIntelFiles(t, intelDir, "test-topic")

	output, err := captureOutput(func() error {
		return intelListCmd(formatMarkdown)
	})

	if err != nil {
		t.Fatalf("intelListCmd failed: %v", err)
	}

	if !strings.Contains(output, "Intel (1 topics)") {
		t.Errorf("expected markdown header, got: %s", output)
	}
	if !strings.Contains(output, "test-topic") {
		t.Errorf("expected topic name in output, got: %s", output)
	}
	if !strings.Contains(output, "items") {
		t.Errorf("expected 'items' text in output, got: %s", output)
	}
}

func TestIntelListJSONFormat(t *testing.T) {
	intelDir, cleanup := setupTempIntelDir(t)
	defer cleanup()

	createTestIntelFiles(t, intelDir, "test-topic")

	output, err := captureOutput(func() error {
		return intelListCmd(formatJSON)
	})

	if err != nil {
		t.Fatalf("intelListCmd failed: %v", err)
	}

	var summaries []map[string]interface{}
	if err := json.Unmarshal([]byte(output), &summaries); err != nil {
		t.Fatalf("failed to parse JSON output: %v", err)
	}

	if len(summaries) != 1 {
		t.Errorf("expected 1 summary, got %d", len(summaries))
	}

	if summaries[0]["name"] != "test-topic" {
		t.Errorf("expected name 'test-topic', got %v", summaries[0]["name"])
	}

	if summaries[0]["items"].(float64) != 3 {
		t.Errorf("expected 3 items, got %v", summaries[0]["items"])
	}
}

func TestIntelListCSVFormat(t *testing.T) {
	intelDir, cleanup := setupTempIntelDir(t)
	defer cleanup()

	createTestIntelFiles(t, intelDir, "test-topic")

	output, err := captureOutput(func() error {
		return intelListCmd(formatCSV)
	})

	if err != nil {
		t.Fatalf("intelListCmd failed: %v", err)
	}

	reader := csv.NewReader(strings.NewReader(output))
	records, err := reader.ReadAll()
	if err != nil {
		t.Fatalf("failed to parse CSV: %v", err)
	}

	if len(records) != 2 {
		t.Errorf("expected 2 rows (header + 1 data), got %d", len(records))
	}

	header := records[0]
	if len(header) < 4 || header[0] != "name" || header[1] != "files" || header[2] != "items" {
		t.Errorf("unexpected CSV header: %v", header)
	}

	data := records[1]
	if data[0] != "test-topic" {
		t.Errorf("expected topic name 'test-topic', got %s", data[0])
	}
	if data[2] != "3" {
		t.Errorf("expected 3 items, got %s", data[2])
	}
}

func TestIntelShowMarkdownFormat(t *testing.T) {
	intelDir, cleanup := setupTempIntelDir(t)
	defer cleanup()

	createTestIntelFiles(t, intelDir, "test-topic")

	output, err := captureOutput(func() error {
		return intelShowCmd("test-topic", time.Time{}, formatMarkdown)
	})

	if err != nil {
		t.Fatalf("intelShowCmd failed: %v", err)
	}

	if !strings.Contains(output, "Intel: test-topic") {
		t.Errorf("expected title in output, got: %s", output)
	}
	if !strings.Contains(output, "First Article") {
		t.Errorf("expected article title in output, got: %s", output)
	}
	if !strings.Contains(output, "John Doe") {
		t.Errorf("expected author in output, got: %s", output)
	}
	if !strings.Contains(output, "https://example.com/article1") {
		t.Errorf("expected URL in output, got: %s", output)
	}
	if !strings.Contains(output, "golang") {
		t.Errorf("expected tags in output, got: %s", output)
	}
}

func TestIntelShowJSONFormat(t *testing.T) {
	intelDir, cleanup := setupTempIntelDir(t)
	defer cleanup()

	createTestIntelFiles(t, intelDir, "test-topic")

	output, err := captureOutput(func() error {
		return intelShowCmd("test-topic", time.Time{}, formatJSON)
	})

	if err != nil {
		t.Fatalf("intelShowCmd failed: %v", err)
	}

	var items []gather.IntelItem
	if err := json.Unmarshal([]byte(output), &items); err != nil {
		t.Fatalf("failed to parse JSON output: %v", err)
	}

	if len(items) != 3 {
		t.Errorf("expected 3 items, got %d", len(items))
	}

	// Check that items are sorted by score descending
	if items[0].Score < items[1].Score {
		t.Errorf("items should be sorted by score descending, got %f then %f", items[0].Score, items[1].Score)
	}

	// Verify all fields are present
	if items[0].ID == "" || items[0].Title == "" || items[0].SourceURL == "" {
		t.Errorf("expected full item data, got: %+v", items[0])
	}
}

func TestIntelShowCSVFormat(t *testing.T) {
	intelDir, cleanup := setupTempIntelDir(t)
	defer cleanup()

	createTestIntelFiles(t, intelDir, "test-topic")

	output, err := captureOutput(func() error {
		return intelShowCmd("test-topic", time.Time{}, formatCSV)
	})

	if err != nil {
		t.Fatalf("intelShowCmd failed: %v", err)
	}

	reader := csv.NewReader(strings.NewReader(output))
	records, err := reader.ReadAll()
	if err != nil {
		t.Fatalf("failed to parse CSV: %v", err)
	}

	if len(records) != 4 {
		t.Errorf("expected 4 rows (header + 3 items), got %d", len(records))
	}

	header := records[0]
	expectedCols := []string{"timestamp", "score", "title", "author", "url", "content", "tags", "engagement"}
	if len(header) != len(expectedCols) {
		t.Errorf("expected %d columns, got %d", len(expectedCols), len(header))
	}

	for i, col := range expectedCols {
		if header[i] != col {
			t.Errorf("column %d: expected %s, got %s", i, col, header[i])
		}
	}

	// Check data row has correct number of columns
	dataRow := records[1]
	if len(dataRow) != len(header) {
		t.Errorf("data row has %d columns, expected %d", len(dataRow), len(header))
	}

	// Verify data is present
	if dataRow[2] == "" {
		t.Errorf("expected title in CSV row, got empty")
	}
}

func TestIntelShowWithSinceFilter(t *testing.T) {
	intelDir, cleanup := setupTempIntelDir(t)
	defer cleanup()

	createTestIntelFiles(t, intelDir, "test-topic")

	// Filter to items from the last 12 hours (should exclude the older files)
	sinceTime := time.Now().Add(-12 * time.Hour)

	output, err := captureOutput(func() error {
		return intelShowCmd("test-topic", sinceTime, formatJSON)
	})

	if err != nil {
		t.Fatalf("intelShowCmd failed: %v", err)
	}

	var items []gather.IntelItem
	if err := json.Unmarshal([]byte(output), &items); err != nil {
		t.Fatalf("failed to parse JSON output: %v", err)
	}

	// Should only have 1 item (the one from the second file that was created just now)
	if len(items) != 1 {
		t.Errorf("expected 1 item with since filter, got %d", len(items))
	}

	if items[0].ID != "item-3" {
		t.Errorf("expected item-3 after filtering, got %s", items[0].ID)
	}
}

func TestIntelShowSinceThenCSV(t *testing.T) {
	intelDir, cleanup := setupTempIntelDir(t)
	defer cleanup()

	createTestIntelFiles(t, intelDir, "test-topic")

	// Filter to items from the last 12 hours in CSV format
	sinceTime := time.Now().Add(-12 * time.Hour)

	output, err := captureOutput(func() error {
		return intelShowCmd("test-topic", sinceTime, formatCSV)
	})

	if err != nil {
		t.Fatalf("intelShowCmd failed: %v", err)
	}

	reader := csv.NewReader(strings.NewReader(output))
	records, err := reader.ReadAll()
	if err != nil {
		t.Fatalf("failed to parse CSV: %v", err)
	}

	// Should have header + 1 data row
	if len(records) != 2 {
		t.Errorf("expected 2 rows (header + 1 item), got %d", len(records))
	}

	// Verify the correct item is present
	if !strings.Contains(records[1][2], "Third Article") {
		t.Errorf("expected 'Third Article' in CSV, got: %v", records[1][2])
	}
}

func TestIntelFormatValidation(t *testing.T) {
	_, cleanup := setupTempIntelDir(t)
	defer cleanup()

	createTestIntelFiles(t, filepath.Join(os.Getenv("SCOUT_CONFIG_DIR"), "intel"), "test-topic")

	// Test invalid format
	err := intelShowCmd("test-topic", time.Time{}, "invalid")
	if err == nil {
		t.Errorf("expected error for invalid format, got nil")
	}

	if !strings.Contains(err.Error(), "invalid format") {
		t.Errorf("expected 'invalid format' error message, got: %v", err)
	}
}

func TestIntelEmptyTopicMarkdown(t *testing.T) {
	intelDir, cleanup := setupTempIntelDir(t)
	defer cleanup()

	// Create empty topic directory
	topicDir := filepath.Join(intelDir, "empty-topic")
	if err := os.MkdirAll(topicDir, 0755); err != nil {
		t.Fatalf("failed to create topic dir: %v", err)
	}

	output, err := captureOutput(func() error {
		return intelShowCmd("empty-topic", time.Time{}, formatMarkdown)
	})

	if err != nil {
		t.Fatalf("intelShowCmd failed: %v", err)
	}

	if !strings.Contains(output, "No intel items found") {
		t.Errorf("expected 'No intel items found' message, got: %s", output)
	}
}

func TestIntelEmptyTopicJSON(t *testing.T) {
	intelDir, cleanup := setupTempIntelDir(t)
	defer cleanup()

	// Create empty topic directory
	topicDir := filepath.Join(intelDir, "empty-topic")
	if err := os.MkdirAll(topicDir, 0755); err != nil {
		t.Fatalf("failed to create topic dir: %v", err)
	}

	output, err := captureOutput(func() error {
		return intelShowCmd("empty-topic", time.Time{}, formatJSON)
	})

	if err != nil {
		t.Fatalf("intelShowCmd failed: %v", err)
	}

	if output != "[]\n" {
		t.Errorf("expected empty JSON array, got: %s", output)
	}
}

func TestIntelEmptyTopicCSV(t *testing.T) {
	intelDir, cleanup := setupTempIntelDir(t)
	defer cleanup()

	// Create empty topic directory
	topicDir := filepath.Join(intelDir, "empty-topic")
	if err := os.MkdirAll(topicDir, 0755); err != nil {
		t.Fatalf("failed to create topic dir: %v", err)
	}

	output, err := captureOutput(func() error {
		return intelShowCmd("empty-topic", time.Time{}, formatCSV)
	})

	if err != nil {
		t.Fatalf("intelShowCmd failed: %v", err)
	}

	reader := csv.NewReader(strings.NewReader(output))
	records, err := reader.ReadAll()
	if err != nil {
		t.Fatalf("failed to parse CSV: %v", err)
	}

	// Should have only header row
	if len(records) != 1 {
		t.Errorf("expected 1 row (header only) for empty topic, got %d", len(records))
	}
}

func TestIntelNonexistentTopic(t *testing.T) {
	_, cleanup := setupTempIntelDir(t)
	defer cleanup()

	err := intelShowCmd("nonexistent", time.Time{}, formatJSON)
	if err == nil {
		t.Errorf("expected error for nonexistent topic, got nil")
	}

	if !strings.Contains(err.Error(), "no intel found") {
		t.Errorf("expected 'no intel found' error message, got: %v", err)
	}
}

func TestIntelCSVTagsFormat(t *testing.T) {
	intelDir, cleanup := setupTempIntelDir(t)
	defer cleanup()

	// Create a topic with items that have multiple tags
	topicDir := filepath.Join(intelDir, "test-topic")
	if err := os.MkdirAll(topicDir, 0755); err != nil {
		t.Fatalf("failed to create topic dir: %v", err)
	}

	file := gather.IntelFile{
		Topic:      "test-topic",
		GatheredAt: time.Now(),
		Source:     "rss",
		Items: []gather.IntelItem{
			{
				ID:        "item-1",
				Title:     "Article with Tags",
				Content:   "Content",
				SourceURL: "https://example.com",
				Author:    "Author",
				Timestamp: time.Now(),
				Tags:      []string{"tag1", "tag2", "tag3"},
				Score:     80.0,
				Engagement: 10,
			},
		},
	}

	writeIntelTestFile(t, topicDir, "test.json", file)

	output, err := captureOutput(func() error {
		return intelShowCmd("test-topic", time.Time{}, formatCSV)
	})

	if err != nil {
		t.Fatalf("intelShowCmd failed: %v", err)
	}

	reader := csv.NewReader(strings.NewReader(output))
	records, err := reader.ReadAll()
	if err != nil {
		t.Fatalf("failed to parse CSV: %v", err)
	}

	// Find tags column (should be index 6)
	tagValue := records[1][6]
	if tagValue != "tag1;tag2;tag3" {
		t.Errorf("expected tags separated by semicolon, got: %s", tagValue)
	}
}
