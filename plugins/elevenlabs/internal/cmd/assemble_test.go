package cmd

import (
	"strings"
	"testing"

	"github.com/joeyhipolito/nanika-elevenlabs/internal/api"
	"github.com/joeyhipolito/nanika-elevenlabs/internal/formatter"
)

func TestDeriveTimingPath(t *testing.T) {
	tests := []struct {
		name  string
		audio string
		want  string
	}{
		{
			name:  "standard voiceover path",
			audio: "/tmp/narration-elevenlabs-voiceover.mp3",
			want:  "/tmp/narration-elevenlabs-timing-map.json",
		},
		{
			name:  "no voiceover suffix",
			audio: "/tmp/narration-elevenlabs.mp3",
			want:  "/tmp/narration-elevenlabs-timing-map.json",
		},
		{
			name:  "relative path",
			audio: "audio/voiceover.mp3",
			want:  "audio/voiceover-timing-map.json",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := deriveTimingPath(tt.audio)
			if got != tt.want {
				t.Errorf("deriveTimingPath(%q) = %q, want %q", tt.audio, got, tt.want)
			}
		})
	}
}

func TestDeriveManifestFromAudio(t *testing.T) {
	tests := []struct {
		name  string
		audio string
		want  string
	}{
		{
			name:  "standard elevenlabs voiceover",
			audio: "/tmp/narration-elevenlabs-voiceover.mp3",
			want:  "/tmp/narration-clip-manifest.json",
		},
		{
			name:  "no elevenlabs suffix",
			audio: "/tmp/narration-voiceover.mp3",
			want:  "/tmp/narration-clip-manifest.json",
		},
		{
			name:  "relative path",
			audio: "audio/my-elevenlabs-voiceover.mp3",
			want:  "audio/my-clip-manifest.json",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := deriveManifestFromAudio(tt.audio)
			if got != tt.want {
				t.Errorf("deriveManifestFromAudio(%q) = %q, want %q", tt.audio, got, tt.want)
			}
		})
	}
}

func TestBuildParts(t *testing.T) {
	tm := &api.TimingMap{
		DurationSeconds: 95.2,
		Pauses: []api.Pause{
			{Index: 0, Start: 14.3, End: 15.1},
			{Index: 1, Start: 24.6, End: 25.3},
			{Index: 2, Start: 33.5, End: 34.0},
		},
	}
	manifest := &formatter.ClipManifest{
		ClipGridSeconds: 8,
		Segments: []formatter.ManifestEntry{
			{Index: 0, Clip: 1, SilenceAfter: 0},
			{Index: 1, Clip: 2, SilenceAfter: 8},
			{Index: 2, Clip: 4, SilenceAfter: 8},
			{Index: 3, Clip: 6, SilenceAfter: 16},
		},
	}

	parts := buildParts(tm, manifest)

	// pause 0: silence_after=0 → no cut, no silence
	// pause 1: silence_after=8 → seg(0→24.6), sil(8), cursor=25.3
	// pause 2: silence_after=8 → seg(25.3→33.5), sil(8), cursor=34.0
	// final seg: (34.0→95.2)
	// trailing: last segment (index 3) silence_after=16 → sil(16)
	expectedCount := 6

	if len(parts) != expectedCount {
		t.Fatalf("expected %d parts, got %d: %+v", expectedCount, len(parts), parts)
	}

	// Verify first segment spans 0 to 24.6 (pause 0 at 14.3 was not cut because silence_after=0).
	if parts[0].isSilence || parts[0].start != 0 || parts[0].end != 24.6 {
		t.Errorf("part[0] = %+v, want speech 0→24.6", parts[0])
	}

	// Verify first silence is 8s.
	if !parts[1].isSilence || parts[1].duration != 8.0 {
		t.Errorf("part[1] = %+v, want silence 8s", parts[1])
	}

	// Verify second speech segment.
	if parts[2].isSilence || parts[2].start != 25.3 || parts[2].end != 33.5 {
		t.Errorf("part[2] = %+v, want speech 25.3→33.5", parts[2])
	}

	// Verify second silence.
	if !parts[3].isSilence || parts[3].duration != 8.0 {
		t.Errorf("part[3] = %+v, want silence 8s", parts[3])
	}

	// Verify final speech segment.
	if parts[4].isSilence || parts[4].start != 34.0 || parts[4].end != 95.2 {
		t.Errorf("part[4] = %+v, want speech 34.0→95.2", parts[4])
	}

	// Verify trailing silence.
	if !parts[5].isSilence || parts[5].duration != 16.0 {
		t.Errorf("part[5] = %+v, want silence 16s", parts[5])
	}
}

func TestBuildPartsNoSilence(t *testing.T) {
	tm := &api.TimingMap{
		DurationSeconds: 30.0,
		Pauses: []api.Pause{
			{Index: 0, Start: 10.0, End: 10.5},
		},
	}
	manifest := &formatter.ClipManifest{
		ClipGridSeconds: 8,
		Segments: []formatter.ManifestEntry{
			{Index: 0, Clip: 1, SilenceAfter: 0},
			{Index: 1, Clip: 2, SilenceAfter: 0},
		},
	}

	parts := buildParts(tm, manifest)

	// No silence_after for any segment → single speech segment 0→30.0.
	if len(parts) != 1 {
		t.Fatalf("expected 1 part, got %d: %+v", len(parts), parts)
	}
	if parts[0].isSilence || parts[0].start != 0 || parts[0].end != 30.0 {
		t.Errorf("part[0] = %+v, want speech 0→30.0", parts[0])
	}
}

func TestBuildFilterComplex(t *testing.T) {
	parts := []assemblePart{
		{start: 0, end: 14.3},
		{isSilence: true, duration: 8.0},
		{start: 15.1, end: 95.2},
	}

	got := buildFilterComplex(parts, 44100, "mono")

	// Verify key substrings are present.
	checks := []string{
		"[0:a]atrim=0.000:14.300,asetpts=PTS-STARTPTS[seg0]",
		"anullsrc=channel_layout=mono:sample_rate=44100,atrim=0:8.000,asetpts=PTS-STARTPTS[sil0]",
		"[0:a]atrim=15.100:95.200,asetpts=PTS-STARTPTS[seg1]",
		"[seg0][sil0][seg1]concat=n=3:v=0:a=1[outa]",
	}
	for _, check := range checks {
		if !strings.Contains(got, check) {
			t.Errorf("filter_complex missing %q\ngot: %s", check, got)
		}
	}
}

func TestComputeTotalDuration(t *testing.T) {
	parts := []assemblePart{
		{start: 0, end: 14.3},
		{isSilence: true, duration: 8.0},
		{start: 15.1, end: 95.2},
		{isSilence: true, duration: 16.0},
	}

	got := computeTotalDuration(parts)
	// 14.3 + 8.0 + 80.1 + 16.0 = 118.4
	want := 118.4
	if diff := got - want; diff > 0.001 || diff < -0.001 {
		t.Errorf("computeTotalDuration() = %f, want %f", got, want)
	}
}

func TestChannelLayoutString(t *testing.T) {
	if got := channelLayoutString(1); got != "mono" {
		t.Errorf("channelLayoutString(1) = %q, want mono", got)
	}
	if got := channelLayoutString(2); got != "stereo" {
		t.Errorf("channelLayoutString(2) = %q, want stereo", got)
	}
	if got := channelLayoutString(6); got != "stereo" {
		t.Errorf("channelLayoutString(6) = %q, want stereo", got)
	}
}

func TestBuildTimeline(t *testing.T) {
	parts := []assemblePart{
		{start: 0, end: 14.3},
		{isSilence: true, duration: 8.0},
		{start: 15.1, end: 33.5},
	}
	manifest := &formatter.ClipManifest{
		ClipGridSeconds: 8,
		Segments: []formatter.ManifestEntry{
			{Index: 0, Clip: 1, SilenceAfter: 8},
			{Index: 1, Clip: 3, SilenceAfter: 0},
		},
	}

	tl := buildTimeline(parts, manifest, "voiceover-assembled.wav", 40.7, 48.0)

	if tl.AudioFile != "voiceover-assembled.wav" {
		t.Errorf("AudioFile = %q, want voiceover-assembled.wav", tl.AudioFile)
	}
	if tl.DurationSeconds != 40.7 {
		t.Errorf("DurationSeconds = %f, want 40.7", tl.DurationSeconds)
	}
	if tl.ExpectedDurationSeconds != 48.0 {
		t.Errorf("ExpectedDurationSeconds = %f, want 48.0", tl.ExpectedDurationSeconds)
	}
	if len(tl.Segments) != 3 {
		t.Fatalf("expected 3 timeline segments, got %d", len(tl.Segments))
	}
	if tl.Segments[0].Type != "speech" {
		t.Errorf("segment[0].Type = %q, want speech", tl.Segments[0].Type)
	}
	if tl.Segments[1].Type != "silence" {
		t.Errorf("segment[1].Type = %q, want silence", tl.Segments[1].Type)
	}
	if tl.Segments[2].Type != "speech" {
		t.Errorf("segment[2].Type = %q, want speech", tl.Segments[2].Type)
	}
	// Verify silence start/end.
	if tl.Segments[1].Start != 14.3 {
		t.Errorf("silence start = %f, want 14.3", tl.Segments[1].Start)
	}
	if tl.Segments[1].End != 22.3 {
		t.Errorf("silence end = %f, want 22.3", tl.Segments[1].End)
	}
}

