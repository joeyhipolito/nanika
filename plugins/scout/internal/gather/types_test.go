package gather

import "testing"

func TestEffectiveMaxItems_Default(t *testing.T) {
	tc := TopicConfig{Name: "test"}
	if got := tc.EffectiveMaxItems(); got != DefaultMaxItemsPerSource {
		t.Errorf("expected default %d, got %d", DefaultMaxItemsPerSource, got)
	}
}

func TestEffectiveMaxItems_Custom(t *testing.T) {
	tc := TopicConfig{Name: "test", MaxItemsPerSource: 25}
	if got := tc.EffectiveMaxItems(); got != 25 {
		t.Errorf("expected 25, got %d", got)
	}
}

func TestEffectiveMaxItems_ZeroUsesDefault(t *testing.T) {
	tc := TopicConfig{Name: "test", MaxItemsPerSource: 0}
	if got := tc.EffectiveMaxItems(); got != DefaultMaxItemsPerSource {
		t.Errorf("expected default %d for zero value, got %d", DefaultMaxItemsPerSource, got)
	}
}
