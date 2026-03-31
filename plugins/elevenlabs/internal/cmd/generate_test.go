package cmd

import (
	"testing"

	"github.com/joeyhipolito/nanika-elevenlabs/internal/api"
)

func TestBuildTimingMapExtractsPauses(t *testing.T) {
	// Simulate alignment data that spells: "hello [pause] world"
	alignment := &api.Alignment{
		Characters:          []string{"h", "e", "l", "l", "o", " ", "[", "p", "a", "u", "s", "e", "]", " ", "w", "o", "r", "l", "d"},
		CharacterStartTimes: []float64{0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0, 1.1, 1.2, 1.3, 1.4, 1.5, 1.6, 1.7, 1.8},
		CharacterEndTimes:   []float64{0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0, 1.1, 1.2, 1.3, 1.4, 1.5, 1.6, 1.7, 1.8, 1.9},
	}

	tm := buildTimingMap("test.mp3", alignment)

	// Should have 3 words: "hello", "[pause]", "world"
	if len(tm.Words) != 3 {
		t.Fatalf("expected 3 words, got %d: %+v", len(tm.Words), tm.Words)
	}
	if tm.Words[0].Word != "hello" {
		t.Errorf("expected word[0]='hello', got %q", tm.Words[0].Word)
	}
	if tm.Words[1].Word != "[pause]" {
		t.Errorf("expected word[1]='[pause]', got %q", tm.Words[1].Word)
	}
	if tm.Words[2].Word != "world" {
		t.Errorf("expected word[2]='world', got %q", tm.Words[2].Word)
	}

	// Should have 1 pause extracted
	if len(tm.Pauses) != 1 {
		t.Fatalf("expected 1 pause, got %d", len(tm.Pauses))
	}
	if tm.Pauses[0].Index != 0 {
		t.Errorf("expected pause index=0, got %d", tm.Pauses[0].Index)
	}
	if tm.Pauses[0].Start != 0.6 {
		t.Errorf("expected pause start=0.6, got %f", tm.Pauses[0].Start)
	}
	if tm.Pauses[0].End != 1.3 {
		t.Errorf("expected pause end=1.3, got %f", tm.Pauses[0].End)
	}
}

func TestBuildTimingMapNoPauses(t *testing.T) {
	alignment := &api.Alignment{
		Characters:          []string{"h", "i"},
		CharacterStartTimes: []float64{0.0, 0.1},
		CharacterEndTimes:   []float64{0.1, 0.2},
	}

	tm := buildTimingMap("test.mp3", alignment)

	if len(tm.Pauses) != 0 {
		t.Errorf("expected 0 pauses, got %d", len(tm.Pauses))
	}
}

func TestBuildTimingMapMultiplePauses(t *testing.T) {
	// "a [pause] b [pause] c"
	alignment := &api.Alignment{
		Characters:          []string{"a", " ", "[", "p", "a", "u", "s", "e", "]", " ", "b", " ", "[", "p", "a", "u", "s", "e", "]", " ", "c"},
		CharacterStartTimes: []float64{0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20},
		CharacterEndTimes:   []float64{1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21},
	}

	tm := buildTimingMap("test.mp3", alignment)

	if len(tm.Pauses) != 2 {
		t.Fatalf("expected 2 pauses, got %d", len(tm.Pauses))
	}
	if tm.Pauses[0].Index != 0 || tm.Pauses[1].Index != 1 {
		t.Errorf("expected pause indices 0,1 got %d,%d", tm.Pauses[0].Index, tm.Pauses[1].Index)
	}
}

func TestFilterSSMLArtifacts(t *testing.T) {
	tm := &api.TimingMap{
		Words: []api.Word{
			{Word: "hello", Start: 0.0, End: 0.5},
			{Word: "<break", Start: 0.6, End: 0.6},
			{Word: "time=\"3.0s\"/>", Start: 0.6, End: 0.6},
			{Word: "time=", Start: 0.7, End: 0.7},
			{Word: "world", Start: 1.0, End: 1.5},
		},
		Sentences: []api.Sentence{
			{Text: "hello world.", Start: 0.0, End: 1.5},
			{Text: "<break time=\"3.0s\"/>", Start: 0.6, End: 0.6},
			{Text: "time=\"3.0s\"/>", Start: 0.7, End: 0.7},
		},
	}

	filterSSMLArtifacts(tm)

	if len(tm.Words) != 2 {
		t.Fatalf("expected 2 words after filtering, got %d: %+v", len(tm.Words), tm.Words)
	}
	if tm.Words[0].Word != "hello" || tm.Words[1].Word != "world" {
		t.Errorf("expected [hello, world], got [%s, %s]", tm.Words[0].Word, tm.Words[1].Word)
	}

	if len(tm.Sentences) != 1 {
		t.Fatalf("expected 1 sentence after filtering, got %d", len(tm.Sentences))
	}
	if tm.Sentences[0].Text != "hello world." {
		t.Errorf("expected 'hello world.', got %q", tm.Sentences[0].Text)
	}
}

func TestFilterSSMLArtifactsNoArtifacts(t *testing.T) {
	tm := &api.TimingMap{
		Words: []api.Word{
			{Word: "hello", Start: 0.0, End: 0.5},
			{Word: "world", Start: 1.0, End: 1.5},
		},
		Sentences: []api.Sentence{
			{Text: "hello world.", Start: 0.0, End: 1.5},
		},
	}

	filterSSMLArtifacts(tm)

	if len(tm.Words) != 2 {
		t.Errorf("expected 2 words unchanged, got %d", len(tm.Words))
	}
	if len(tm.Sentences) != 1 {
		t.Errorf("expected 1 sentence unchanged, got %d", len(tm.Sentences))
	}
}

func TestFilterSSMLArtifactsNilPauses(t *testing.T) {
	// Filtering should not affect pauses
	tm := &api.TimingMap{
		Words: []api.Word{
			{Word: "<break", Start: 0.0, End: 0.0},
		},
		Sentences: []api.Sentence{},
		Pauses: []api.Pause{
			{Index: 0, Start: 1.0, End: 1.5},
		},
	}

	filterSSMLArtifacts(tm)

	if len(tm.Words) != 0 {
		t.Errorf("expected 0 words after filtering, got %d", len(tm.Words))
	}
	if len(tm.Pauses) != 1 {
		t.Errorf("pauses should be unaffected, got %d", len(tm.Pauses))
	}
}
