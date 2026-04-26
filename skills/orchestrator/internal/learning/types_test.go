package learning

import "testing"

func TestShortID(t *testing.T) {
	tests := []struct {
		name string
		id   string
		want string
	}{
		{name: "nanosecond suffix returns trailing 8", id: "learn_1770676804944537000", want: "44537000"},
		{name: "hex suffix returns trailing 8", id: "docs_023769f43151a3e901705c12d428280a", want: "d428280a"},
		{name: "no underscore returns whole id", id: "trk-BD4D", want: "trk-BD4D"},
		{name: "empty id returns empty string", id: "", want: ""},
		{name: "underscore with short suffix returns whole suffix", id: "x_abc", want: "abc"},
		{name: "underscore with exactly-8 suffix returns whole suffix", id: "x_12345678", want: "12345678"},
		{name: "trailing underscore returns empty suffix", id: "learn_", want: ""},
		{name: "only first underscore splits the id", id: "a_b_cdefghij", want: "cdefghij"},
		{name: "whole-id shorter than 8 returns whole id", id: "ab-12", want: "ab-12"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			l := Learning{ID: tt.id}
			if got := l.ShortID(); got != tt.want {
				t.Errorf("ShortID() = %q, want %q", got, tt.want)
			}
		})
	}
}

// TestShortID_NoCollisionForCloseNanoIDs regresses the blocker fixed in this
// change: two learn_<UnixNano> IDs created within a 100-second window used to
// share a ShortID because the first 8 digits of UnixNano are constant across
// a 10^11ns span. The fix switches to trailing 8 chars, which advance every
// nanosecond tick.
func TestShortID_NoCollisionForCloseNanoIDs(t *testing.T) {
	a := Learning{ID: "learn_1770676804944537000"}
	b := Learning{ID: "learn_1770676804944537001"} // 1 ns later
	c := Learning{ID: "learn_1770676899999999999"} // ~95s later, same first-8 prefix

	if a.ShortID() == b.ShortID() {
		t.Errorf("ShortID collision for 1ns-apart IDs: %q == %q", a.ShortID(), b.ShortID())
	}
	if a.ShortID() == c.ShortID() {
		t.Errorf("ShortID collision for same-prefix IDs (%q, %q): %q == %q",
			a.ID, c.ID, a.ShortID(), c.ShortID())
	}
}
