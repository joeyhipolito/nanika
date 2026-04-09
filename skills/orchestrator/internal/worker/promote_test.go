package worker

import (
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
)

func TestConvertLearningType(t *testing.T) {
	tests := []struct {
		input learning.LearningType
		want  string
	}{
		{learning.TypePattern, "pattern"},
		{learning.TypeError, "feedback"},
		{learning.TypeSource, "reference"},
		{learning.TypeDecision, "feedback"},
		{learning.TypeInsight, "user"},
	}

	for _, tt := range tests {
		got := convertLearningType(tt.input)
		if got != tt.want {
			t.Errorf("convertLearningType(%q) = %q, want %q", tt.input, got, tt.want)
		}
	}
}
