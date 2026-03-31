// Package api provides types and HTTP client for the ElevenLabs REST API.
package api

// Voice represents a single voice from the /v1/voices endpoint.
type Voice struct {
	VoiceID  string            `json:"voice_id"`
	Name     string            `json:"name"`
	Category string            `json:"category"`
	Labels   map[string]string `json:"labels"`
}

// VoicesResponse is the response body for GET /v1/voices.
type VoicesResponse struct {
	Voices []Voice `json:"voices"`
}

// UserResponse is the response body for GET /v1/user (used to verify API key).
type UserResponse struct {
	Subscription Subscription `json:"subscription"`
}

// Subscription holds quota information from the /v1/user endpoint.
type Subscription struct {
	CharacterCount     int    `json:"character_count"`
	CharacterLimit     int    `json:"character_limit"`
	Status             string `json:"status"`
	NextCharacterCountResetUnix int64 `json:"next_character_count_reset_unix"`
}

// TTSRequest is the request body for POST /v1/text-to-speech/{voice_id}/with-timestamps.
type TTSRequest struct {
	Text          string        `json:"text"`
	ModelID       string        `json:"model_id"`
	VoiceSettings VoiceSettings `json:"voice_settings"`
	Seed          int32         `json:"seed,omitempty"`
}

// VoiceSettings controls prosody parameters for the TTS request.
type VoiceSettings struct {
	Stability       float64 `json:"stability"`
	SimilarityBoost float64 `json:"similarity_boost"`
	Style           float64 `json:"style"`
	Speed           float64 `json:"speed,omitempty"`
}

// TTSResponse is the response body for the with-timestamps TTS endpoint.
// The audio bytes are returned as a base64-encoded field; words carry timing data.
type TTSResponse struct {
	AudioBase64 string      `json:"audio_base64"`
	Alignment   *Alignment  `json:"alignment"`
}

// Alignment holds word-level timing data from the TTS response.
type Alignment struct {
	Characters         []string  `json:"characters"`
	CharacterStartTimes []float64 `json:"character_start_times_seconds"`
	CharacterEndTimes   []float64 `json:"character_end_times_seconds"`
}

// Pause holds the position of a [pause] marker in the timing map.
type Pause struct {
	Index int     `json:"index"` // 0-based pause index
	Start float64 `json:"start"` // start time in seconds
	End   float64 `json:"end"`   // end time in seconds
}

// TimingMap is the output format written to timing-map.json.
type TimingMap struct {
	AudioFile       string     `json:"audio_file"`
	DurationSeconds float64    `json:"duration_seconds"`
	Words           []Word     `json:"words"`
	Sentences       []Sentence `json:"sentences"`
	Pauses          []Pause    `json:"pauses,omitempty"`
}

// Word holds a single word and its start/end timestamps in seconds.
type Word struct {
	Word  string  `json:"word"`
	Start float64 `json:"start"`
	End   float64 `json:"end"`
}

// Sentence holds a narration sentence and its timestamp range.
type Sentence struct {
	Text  string  `json:"text"`
	Start float64 `json:"start"`
	End   float64 `json:"end"`
}
