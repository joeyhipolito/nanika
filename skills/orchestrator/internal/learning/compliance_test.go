package learning

import (
	"testing"
)

func TestExtractKeywords(t *testing.T) {
	tests := []struct {
		name    string
		text    string
		wantMin int // minimum expected keywords
	}{
		{
			name:    "empty text",
			text:    "",
			wantMin: 0,
		},
		{
			name:    "all short words",
			text:    "use the and if or",
			wantMin: 0,
		},
		{
			name:    "normal learning",
			text:    "Always wrap errors with context using fmt.Errorf to preserve the error chain.",
			wantMin: 3,
		},
		{
			name:    "deduplicates words",
			text:    "errors errors errors wrap wrap",
			wantMin: 1,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			kw := extractKeywords(tc.text)
			if len(kw) < tc.wantMin {
				t.Errorf("extractKeywords(%q): got %d keywords, want >= %d", tc.text, len(kw), tc.wantMin)
			}
		})
	}
}

func TestLearningFollowed(t *testing.T) {
	tests := []struct {
		name    string
		content string
		output  string
		want    bool
	}{
		{
			name:    "keywords clearly present",
			content: "Always wrap errors with context using fmt.Errorf to preserve the chain.",
			output:  "I wrapped the error using fmt.Errorf to preserve the error chain and context.",
			want:    true,
		},
		{
			name:    "keywords absent",
			content: "Always wrap errors with context using fmt.Errorf to preserve the chain.",
			output:  "The weather is nice today and the task is complete.",
			want:    false,
		},
		{
			name:    "partial match below threshold",
			content: "Use dependency injection to decouple components and improve testability.",
			output:  "injection was considered but not implemented in this phase.",
			want:    false,
		},
		{
			name:    "empty content",
			content: "",
			output:  "some output text that is clearly present",
			want:    false,
		},
		{
			name:    "empty output",
			content: "Always check errors explicitly in every function call.",
			output:  "",
			want:    false,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := learningFollowed(tc.content, tc.output)
			if got != tc.want {
				t.Errorf("learningFollowed(%q, %q) = %v, want %v",
					tc.content, tc.output, got, tc.want)
			}
		})
	}
}

func TestComplianceScan(t *testing.T) {
	learnings := []Learning{
		{
			ID:      "learn_1",
			Content: "Always wrap errors with context using fmt.Errorf to preserve chain.",
		},
		{
			ID:      "learn_2",
			Content: "Use dependency injection to decouple components for testability.",
		},
	}

	// Output contains clear keywords for learn_1, not for learn_2
	output := `
I used fmt.Errorf to wrap errors with context in every function.
The error chain is preserved throughout the codebase.
No injection pattern was used here.
`

	result := ComplianceScan(learnings, output)

	if !result["learn_1"] {
		t.Error("expected learn_1 (error wrapping) to be followed, but it wasn't")
	}
	if result["learn_2"] {
		t.Error("expected learn_2 (dependency injection) not to be followed, but it was")
	}
}
