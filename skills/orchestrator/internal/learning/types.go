// Package learning provides learning capture and retrieval for the orchestrator.
package learning

import (
	"strings"
	"time"
)

// LearningType categorizes the type of learning
type LearningType string

const (
	TypeInsight    LearningType = "insight"
	TypePattern    LearningType = "pattern"
	TypeError      LearningType = "error"
	TypeSource     LearningType = "source"
	TypeDecision   LearningType = "decision"
)

// Learning represents a captured learning
type Learning struct {
	ID           string                 `json:"id"`
	Type         LearningType           `json:"type"`
	Content      string                 `json:"content"`
	Context      string                 `json:"context,omitempty"`
	Domain       string                 `json:"domain"`
	WorkerName   string                 `json:"worker_name"`
	WorkspaceID  string                 `json:"workspace_id"`
	Marker       string                 `json:"marker,omitempty"` // original marker prefix, e.g. "FINDING:"
	Tags         []string               `json:"tags,omitempty"`
	CreatedAt    time.Time              `json:"created_at"`
	SeenCount    int                    `json:"seen_count,omitempty"`
	UsageCount   int                    `json:"usage_count"`
	QualityScore float64                `json:"quality_score,omitempty"`
	LastUsedAt   *time.Time             `json:"last_used_at,omitempty"`
	Embedding    []float32              `json:"-"`

	// Compliance tracking: updated when learnings are injected and scanned post-mission.
	InjectionCount  int     `json:"injection_count,omitempty"`
	ComplianceCount int     `json:"compliance_count,omitempty"`
	ComplianceRate  float64 `json:"compliance_rate,omitempty"`
}

// ShortID returns a short, human-friendly identifier for citation. Takes
// the last 8 characters of the ID's suffix after the first underscore (or
// the whole ID if no underscore, or if the suffix is shorter than 8). Used
// in the worker-facing injection format and in ParseCitedLearnings.
//
// The last 8 are chosen (not the first 8) so that IDs of the form
// `learn_<UnixNano>` don't collide: the leading digits of a nanosecond
// timestamp are shared for any two IDs created within ~100s, while the
// trailing digits change on every-nanosecond tick.
func (l Learning) ShortID() string {
	id := l.ID
	if _, after, ok := strings.Cut(id, "_"); ok {
		id = after
	}
	if len(id) > 8 {
		id = id[len(id)-8:]
	}
	return id
}

// MarkerConfig defines a pattern to extract from worker output
type MarkerConfig struct {
	Marker string       `json:"marker"` // e.g., "LEARNING:"
	Type   LearningType `json:"type"`
}

// DefaultMarkers are the standard markers all workers should use
var DefaultMarkers = []MarkerConfig{
	{Marker: "LEARNING:", Type: TypeInsight},
	{Marker: "TIL:", Type: TypeInsight},
	{Marker: "INSIGHT:", Type: TypeInsight},
	{Marker: "FINDING:", Type: TypeInsight},
	{Marker: "PATTERN:", Type: TypePattern},
	{Marker: "APPROACH:", Type: TypePattern},
	{Marker: "GOTCHA:", Type: TypeError},
	// ERROR: intentionally omitted — collides with log output
	// (e.g., "ERROR: context deadline exceeded after 30s" is 47 chars,
	// passes validation, and gets stored as a TypeError learning).
	{Marker: "FIX:", Type: TypeError},
	{Marker: "SOURCE:", Type: TypeSource},
	{Marker: "DECISION:", Type: TypeDecision},
	{Marker: "TRADEOFF:", Type: TypeDecision},
}
