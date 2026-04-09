package store

import (
	"bufio"
	"bytes"
	"encoding/gob"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"os"
	"sort"
	"strings"
	"time"
	"unicode"

	"github.com/joeyhipolito/nanika-memory/internal/config"
)

const snapshotVersion = 1

// AddInput describes one write into memory.
type AddInput struct {
	Text   string
	Entity string
	Slots  map[string]string
	Tags   map[string]string
	Source string
}

// Entry is a single append-only memory record.
type Entry struct {
	ID        uint64            `json:"id"`
	CreatedAt time.Time         `json:"created_at"`
	Text      string            `json:"text"`
	Entity    string            `json:"entity,omitempty"`
	Source    string            `json:"source,omitempty"`
	Slots     map[string]string `json:"slots,omitempty"`
	Tags      map[string]string `json:"tags,omitempty"`
	Terms     []string          `json:"terms,omitempty"`
	// Trust is the cumulative trust score for this entry (default 1.0 when absent).
	// Adjusted by "helpful" (+0.05) and "unhelpful" (-0.10) signals.
	// Old entries without this field load as 0.0 and are treated as 1.0.
	Trust float64 `json:"trust,omitempty"`
	// Signal and RefID are set on trust-signal entries only.
	Signal string `json:"signal,omitempty"`
	RefID  uint64 `json:"ref_id,omitempty"`
}

// EntityState is the direct slot table for one entity.
type EntityState struct {
	Entity    string            `json:"entity"`
	Slots     map[string]string `json:"slots"`
	UpdatedAt time.Time         `json:"updated_at"`
	Evidence  []uint64          `json:"evidence"`
}

// SearchHit is one ranked retrieval result.
type SearchHit struct {
	ID            uint64            `json:"id"`
	CreatedAt     time.Time         `json:"created_at"`
	Score         float64           `json:"score"`
	TokenScore    float64           `json:"token_score"`
	Jaccard       float64           `json:"jaccard"`
	TemporalDecay float64           `json:"temporal_decay"`
	Trust         float64           `json:"trust"`
	FinalScore    float64           `json:"final_score"`
	Entity        string            `json:"entity,omitempty"`
	Source        string            `json:"source,omitempty"`
	Text          string            `json:"text"`
	Preview       string            `json:"preview"`
	Slots         map[string]string `json:"slots,omitempty"`
	Tags          map[string]string `json:"tags,omitempty"`
}

// SearchResult is the public search response.
type SearchResult struct {
	Query string      `json:"query"`
	Hits  []SearchHit `json:"hits"`
	Count int         `json:"count"`
}

// Stats summarises the current store.
type Stats struct {
	OK           bool      `json:"ok"`
	StoreDir     string    `json:"store_dir"`
	LogPath      string    `json:"log_path"`
	SnapshotPath string    `json:"snapshot_path"`
	EntryCount   int       `json:"entry_count"`
	EntityCount  int       `json:"entity_count"`
	TokenCount   int       `json:"token_count"`
	LastUpdated  time.Time `json:"last_updated,omitempty"`
}

type snapshot struct {
	Version     int
	NextID      uint64
	UpdatedAt   time.Time
	Entries     map[uint64]Entry
	EntryOrder  []uint64
	TokenIndex  map[string][]uint64
	EntityIndex map[string][]uint64
	FacetIndex  map[string][]uint64
	States      map[string]EntityState
	TrustIndex  map[uint64][]string
}

// Engine provides read and write access to the compiled memory snapshot.
type Engine struct {
	snapshot snapshot
}

// Open loads the compiled index, rebuilding from the append-only log when needed.
func Open() (*Engine, error) {
	if err := config.EnsureStoreDir(); err != nil {
		return nil, fmt.Errorf("ensure store dir: %w", err)
	}

	snapshotPath := config.SnapshotPath()
	if _, err := os.Stat(snapshotPath); err == nil {
		snap, loadErr := loadSnapshot(snapshotPath)
		if loadErr == nil {
			return &Engine{snapshot: snap}, nil
		}
		if _, statErr := os.Stat(config.LogPath()); errors.Is(statErr, os.ErrNotExist) {
			return nil, fmt.Errorf("load snapshot: %w", loadErr)
		}
	}

	if _, err := os.Stat(config.LogPath()); err == nil {
		return Rebuild()
	}

	return &Engine{snapshot: newSnapshot()}, nil
}

// Rebuild reconstructs the compiled index from the append-only log.
// Trust fields on entries are populated from trust-signal entries during the
// replay, so rebuilt entries carry the correct cumulative trust value.
func Rebuild() (*Engine, error) {
	if err := config.EnsureStoreDir(); err != nil {
		return nil, fmt.Errorf("ensure store dir: %w", err)
	}

	snap := newSnapshot()
	logFile, err := os.Open(config.LogPath())
	if errors.Is(err, os.ErrNotExist) {
		engine := &Engine{snapshot: snap}
		if err := engine.save(); err != nil {
			return nil, err
		}
		return engine, nil
	}
	if err != nil {
		return nil, fmt.Errorf("open log: %w", err)
	}
	defer logFile.Close()

	scanner := bufio.NewScanner(logFile)
	scanner.Buffer(make([]byte, 0, 64*1024), 4*1024*1024)
	for scanner.Scan() {
		line := bytes.TrimSpace(scanner.Bytes())
		if len(line) == 0 {
			continue
		}
		var entry Entry
		if err := json.Unmarshal(line, &entry); err != nil {
			return nil, fmt.Errorf("decode log entry: %w", err)
		}
		entry = normalizeEntry(entry)
		snap.addEntry(entry)
	}
	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("scan log: %w", err)
	}

	engine := &Engine{snapshot: snap}
	if err := engine.save(); err != nil {
		return nil, err
	}
	return engine, nil
}

// Add appends a new memory record and incrementally updates the compiled index.
func (e *Engine) Add(input AddInput) (Entry, EntityState, error) {
	input.Text = strings.TrimSpace(input.Text)
	input.Entity = strings.TrimSpace(input.Entity)
	input.Source = strings.TrimSpace(input.Source)
	input.Slots = cloneMap(input.Slots)
	input.Tags = cloneMap(input.Tags)

	if input.Entity == "" && len(input.Slots) > 0 {
		return Entry{}, EntityState{}, fmt.Errorf("slots require --entity or remember <entity>")
	}
	if input.Text == "" && len(input.Slots) == 0 {
		return Entry{}, EntityState{}, fmt.Errorf("memory entry needs text or at least one slot")
	}

	entry := normalizeEntry(Entry{
		ID:        e.snapshot.NextID + 1,
		CreatedAt: time.Now().UTC(),
		Text:      input.Text,
		Entity:    input.Entity,
		Source:    input.Source,
		Slots:     input.Slots,
		Tags:      input.Tags,
	})

	if err := appendToLog(entry); err != nil {
		return Entry{}, EntityState{}, err
	}

	e.snapshot.addEntry(entry)
	if err := e.save(); err != nil {
		return Entry{}, EntityState{}, err
	}

	var state EntityState
	if entry.Entity != "" {
		state, _ = e.State(entry.Entity)
	}
	return entry, state, nil
}

// Find searches the compiled symbolic index using a multi-signal scoring pipeline:
//  1. Token score: fraction of query tokens present in the entry (0–1)
//  2. Jaccard similarity: |query∩entry| / |query∪entry| (0–1)
//  3. Temporal decay: 0.5^(age_days / half_life) — recent entries score higher
//  4. Trust multiplier: entry.Trust (default 1.0; adjusted by trust signals)
//
// Final score = (token_score×0.5 + jaccard×0.5) × decay × trust
// Entity exact-match entries receive a 1.5× boost to the base signal.
func (e *Engine) Find(query string, top int) SearchResult {
	if top <= 0 {
		top = 5
	}

	query = strings.TrimSpace(query)
	parsed := parseQuery(query)
	halfLife := config.DecayHalfLifeDays()
	now := time.Now().UTC()

	// Build query token set for Jaccard computation.
	queryTokenSet := make(map[string]struct{}, len(parsed.Tokens))
	for _, t := range parsed.Tokens {
		queryTokenSet[t] = struct{}{}
	}

	// Resolve facet candidates — must ALL match (AND semantics).
	var facetCandidates []uint64
	for _, facet := range parsed.Facets {
		postings := e.snapshot.FacetIndex[facet]
		if len(postings) == 0 {
			return SearchResult{Query: query, Hits: nil, Count: 0}
		}
		if facetCandidates == nil {
			facetCandidates = cloneIDs(postings)
			continue
		}
		facetCandidates = intersectIDs(facetCandidates, postings)
		if len(facetCandidates) == 0 {
			return SearchResult{Query: query, Hits: nil, Count: 0}
		}
	}

	// Collect candidate entry IDs from token index and entity index.
	candidateSet := make(map[uint64]struct{})
	for _, token := range parsed.Tokens {
		for _, id := range e.snapshot.TokenIndex[token] {
			candidateSet[id] = struct{}{}
		}
	}
	if parsed.ExactEntity != "" {
		for _, id := range e.snapshot.EntityIndex[parsed.ExactEntity] {
			candidateSet[id] = struct{}{}
		}
	}
	// Facet-only query: seed candidates from facets.
	if len(candidateSet) == 0 && len(facetCandidates) > 0 {
		for _, id := range facetCandidates {
			candidateSet[id] = struct{}{}
		}
	}

	var facetSet map[uint64]struct{}
	if len(facetCandidates) > 0 {
		facetSet = make(map[uint64]struct{}, len(facetCandidates))
		for _, id := range facetCandidates {
			facetSet[id] = struct{}{}
		}
	}

	hits := make([]SearchHit, 0, len(candidateSet))
	for id := range candidateSet {
		if facetSet != nil {
			if _, ok := facetSet[id]; !ok {
				continue
			}
		}
		entry, ok := e.snapshot.Entries[id]
		if !ok {
			continue
		}
		// Skip trust-signal meta-entries from content results.
		if entry.Signal != "" {
			continue
		}

		// Token score: fraction of query tokens matched in this entry.
		var tokenScore float64
		if len(parsed.Tokens) == 0 {
			tokenScore = 1.0
		} else {
			entryTermSet := make(map[string]struct{}, len(entry.Terms))
			for _, t := range entry.Terms {
				entryTermSet[t] = struct{}{}
			}
			matches := 0
			for _, t := range parsed.Tokens {
				if _, found := entryTermSet[t]; found {
					matches++
				}
			}
			tokenScore = float64(matches) / float64(len(parsed.Tokens))
		}

		// Jaccard similarity between query token set and entry term set.
		jaccard := jaccardSim(queryTokenSet, entry.Terms)

		// Base signal: weighted average of token score and Jaccard.
		base := tokenScore*0.5 + jaccard*0.5

		// Entity exact-match boost.
		if parsed.ExactEntity != "" && normalizePhrase(entry.Entity) == parsed.ExactEntity {
			base = math.Min(1.0, base*1.5)
		}

		// Temporal decay: 0.5^(age / half_life).
		decay := temporalDecay(entry.CreatedAt, halfLife, now)

		// Trust multiplier: treat missing/zero Trust as 1.0 (backward compatible).
		trust := effectiveTrust(entry.Trust)

		score := base * decay * trust

		hits = append(hits, SearchHit{
			ID:            entry.ID,
			CreatedAt:     entry.CreatedAt,
			Score:         score,
			TokenScore:    tokenScore,
			Jaccard:       jaccard,
			TemporalDecay: decay,
			Trust:         trust,
			FinalScore:    score,
			Entity:        entry.Entity,
			Source:        entry.Source,
			Text:          entry.Text,
			Preview:       preview(entry.Text),
			Slots:         cloneMap(entry.Slots),
			Tags:          cloneMap(entry.Tags),
		})
	}

	sort.Slice(hits, func(i, j int) bool {
		if hits[i].Score != hits[j].Score {
			return hits[i].Score > hits[j].Score
		}
		if !hits[i].CreatedAt.Equal(hits[j].CreatedAt) {
			return hits[i].CreatedAt.After(hits[j].CreatedAt)
		}
		return hits[i].ID > hits[j].ID
	})

	if len(hits) > top {
		hits = hits[:top]
	}
	return SearchResult{
		Query: query,
		Hits:  hits,
		Count: len(hits),
	}
}

// State returns the direct slot state for an entity.
func (e *Engine) State(entity string) (EntityState, bool) {
	state, ok := e.snapshot.States[normalizePhrase(entity)]
	if !ok {
		return EntityState{}, false
	}
	state.Slots = cloneMap(state.Slots)
	state.Evidence = cloneIDs(state.Evidence)
	return state, true
}

// Recent returns the most recent entries first.
func (e *Engine) Recent(limit int) []Entry {
	if limit <= 0 {
		limit = 10
	}
	result := make([]Entry, 0, limit)
	for i := len(e.snapshot.EntryOrder) - 1; i >= 0 && len(result) < limit; i-- {
		id := e.snapshot.EntryOrder[i]
		entry, ok := e.snapshot.Entries[id]
		if !ok {
			continue
		}
		entry.Slots = cloneMap(entry.Slots)
		entry.Tags = cloneMap(entry.Tags)
		entry.Terms = append([]string(nil), entry.Terms...)
		result = append(result, entry)
	}
	return result
}

// Stats returns a current summary of the store.
func (e *Engine) Stats() Stats {
	return Stats{
		OK:           true,
		StoreDir:     config.StoreDir(),
		LogPath:      config.LogPath(),
		SnapshotPath: config.SnapshotPath(),
		EntryCount:   len(e.snapshot.EntryOrder),
		EntityCount:  len(e.snapshot.States),
		TokenCount:   len(e.snapshot.TokenIndex),
		LastUpdated:  e.snapshot.UpdatedAt,
	}
}

// Trust appends a trust-signal entry for an existing entry and updates its
// cumulative trust score in the snapshot.
//
// Valid signals:
//   - "helpful"   → trust += 0.05
//   - "unhelpful" → trust -= 0.10 (floored at 0)
//
// Each signal type can only be applied once per entry (subsequent duplicates
// are silently ignored to prevent runaway inflation).
func (e *Engine) Trust(id uint64, signal string) (Entry, error) {
	if _, ok := e.snapshot.Entries[id]; !ok {
		return Entry{}, fmt.Errorf("entry %d not found", id)
	}
	switch signal {
	case "helpful", "unhelpful":
	default:
		return Entry{}, fmt.Errorf("unknown signal %q: valid signals are: helpful, unhelpful", signal)
	}

	entry := normalizeEntry(Entry{
		ID:        e.snapshot.NextID + 1,
		CreatedAt: time.Now().UTC(),
		Text:      fmt.Sprintf("trust signal %s for entry %d", signal, id),
		Signal:    signal,
		RefID:     id,
	})

	if err := appendToLog(entry); err != nil {
		return Entry{}, err
	}
	e.snapshot.addEntry(entry)
	if err := e.save(); err != nil {
		return Entry{}, err
	}
	return entry, nil
}

// TrustSignals returns all trust signals recorded for an entry.
func (e *Engine) TrustSignals(id uint64) []string {
	signals := e.snapshot.TrustIndex[id]
	if len(signals) == 0 {
		return nil
	}
	out := make([]string, len(signals))
	copy(out, signals)
	return out
}

func newSnapshot() snapshot {
	return snapshot{
		Version:     snapshotVersion,
		Entries:     map[uint64]Entry{},
		TokenIndex:  map[string][]uint64{},
		EntityIndex: map[string][]uint64{},
		FacetIndex:  map[string][]uint64{},
		States:      map[string]EntityState{},
		TrustIndex:  map[uint64][]string{},
	}
}

func loadSnapshot(path string) (snapshot, error) {
	file, err := os.Open(path)
	if err != nil {
		return snapshot{}, fmt.Errorf("open snapshot: %w", err)
	}
	defer file.Close()

	var snap snapshot
	if err := gob.NewDecoder(file).Decode(&snap); err != nil {
		return snapshot{}, fmt.Errorf("decode snapshot: %w", err)
	}
	if snap.Version != snapshotVersion {
		return snapshot{}, fmt.Errorf("unsupported snapshot version %d", snap.Version)
	}
	if snap.Entries == nil {
		snap.Entries = map[uint64]Entry{}
	}
	if snap.TokenIndex == nil {
		snap.TokenIndex = map[string][]uint64{}
	}
	if snap.EntityIndex == nil {
		snap.EntityIndex = map[string][]uint64{}
	}
	if snap.FacetIndex == nil {
		snap.FacetIndex = map[string][]uint64{}
	}
	if snap.States == nil {
		snap.States = map[string]EntityState{}
	}
	if snap.TrustIndex == nil {
		snap.TrustIndex = map[uint64][]string{}
	}
	return snap, nil
}

func (e *Engine) save() error {
	if err := config.EnsureStoreDir(); err != nil {
		return fmt.Errorf("ensure store dir: %w", err)
	}

	tmpPath := config.SnapshotPath() + ".tmp"
	file, err := os.Create(tmpPath)
	if err != nil {
		return fmt.Errorf("create snapshot: %w", err)
	}

	enc := gob.NewEncoder(file)
	if err := enc.Encode(e.snapshot); err != nil {
		file.Close()
		return fmt.Errorf("encode snapshot: %w", err)
	}
	if err := file.Close(); err != nil {
		return fmt.Errorf("close snapshot: %w", err)
	}
	if err := os.Rename(tmpPath, config.SnapshotPath()); err != nil {
		return fmt.Errorf("replace snapshot: %w", err)
	}
	return nil
}

func appendToLog(entry Entry) error {
	if err := config.EnsureStoreDir(); err != nil {
		return fmt.Errorf("ensure store dir: %w", err)
	}

	file, err := os.OpenFile(config.LogPath(), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return fmt.Errorf("open log: %w", err)
	}
	defer file.Close()

	if err := json.NewEncoder(file).Encode(entry); err != nil {
		return fmt.Errorf("append log: %w", err)
	}
	return nil
}

func (s *snapshot) addEntry(entry Entry) {
	if entry.ID == 0 {
		entry.ID = s.NextID + 1
	}
	if entry.CreatedAt.IsZero() {
		entry.CreatedAt = time.Now().UTC()
	}
	if entry.Terms == nil {
		entry.Terms = collectTerms(entry)
	}

	if _, exists := s.Entries[entry.ID]; exists {
		return
	}

	s.NextID = maxID(s.NextID, entry.ID)
	if entry.CreatedAt.After(s.UpdatedAt) {
		s.UpdatedAt = entry.CreatedAt
	}
	s.Entries[entry.ID] = entry
	s.EntryOrder = append(s.EntryOrder, entry.ID)

	// Trust-signal entries: apply the delta to the referenced entry's Trust
	// field and update TrustIndex. The signal entry itself stays in Entries
	// (for log/recent) but is excluded from Find() results.
	if entry.Signal != "" && entry.RefID != 0 {
		signals := s.TrustIndex[entry.RefID]
		for _, sig := range signals {
			if sig == entry.Signal {
				return // duplicate signal; already applied
			}
		}
		s.TrustIndex[entry.RefID] = append(signals, entry.Signal)

		// Update the referenced entry's cumulative trust score.
		if ref, ok := s.Entries[entry.RefID]; ok {
			t := effectiveTrust(ref.Trust)
			switch entry.Signal {
			case "helpful":
				t += 0.05
			case "unhelpful":
				t -= 0.10
				if t < 0 {
					t = 0
				}
			}
			ref.Trust = t
			s.Entries[entry.RefID] = ref
		}
		return
	}

	for _, term := range entry.Terms {
		s.TokenIndex[term] = appendID(s.TokenIndex[term], entry.ID)
	}

	entityKey := normalizePhrase(entry.Entity)
	if entityKey != "" {
		s.EntityIndex[entityKey] = appendID(s.EntityIndex[entityKey], entry.ID)
		s.FacetIndex["entity="+entityKey] = appendID(s.FacetIndex["entity="+entityKey], entry.ID)

		state := s.States[entityKey]
		if state.Entity == "" {
			state.Entity = entry.Entity
		}
		if state.Slots == nil {
			state.Slots = map[string]string{}
		}
		keys := sortedKeys(entry.Slots)
		for _, key := range keys {
			normKey := normalizePhrase(key)
			value := strings.TrimSpace(entry.Slots[key])
			if normKey == "" || value == "" {
				continue
			}
			state.Slots[normKey] = value
			normValue := normalizePhrase(value)
			slotFacet := "slot." + normKey + "=" + normValue
			s.FacetIndex[slotFacet] = appendID(s.FacetIndex[slotFacet], entry.ID)
			s.FacetIndex[normKey+"="+normValue] = appendID(s.FacetIndex[normKey+"="+normValue], entry.ID)
		}
		state.UpdatedAt = entry.CreatedAt
		state.Evidence = appendID(state.Evidence, entry.ID)
		s.States[entityKey] = state
	}

	for _, key := range sortedKeys(entry.Tags) {
		normKey := normalizePhrase(key)
		normValue := normalizePhrase(entry.Tags[key])
		if normKey == "" || normValue == "" {
			continue
		}
		tagFacet := "tag." + normKey + "=" + normValue
		s.FacetIndex[tagFacet] = appendID(s.FacetIndex[tagFacet], entry.ID)
		s.FacetIndex[normKey+"="+normValue] = appendID(s.FacetIndex[normKey+"="+normValue], entry.ID)
	}

	dayFacet := "day=" + entry.CreatedAt.UTC().Format("2006-01-02")
	s.FacetIndex[dayFacet] = appendID(s.FacetIndex[dayFacet], entry.ID)
}

func normalizeEntry(entry Entry) Entry {
	entry.Text = strings.TrimSpace(entry.Text)
	entry.Entity = strings.TrimSpace(entry.Entity)
	entry.Source = strings.TrimSpace(entry.Source)
	entry.Slots = cloneMap(entry.Slots)
	entry.Tags = cloneMap(entry.Tags)
	if entry.CreatedAt.IsZero() {
		entry.CreatedAt = time.Now().UTC()
	}
	if entry.Text == "" && entry.Entity != "" && len(entry.Slots) > 0 {
		entry.Text = syntheticText(entry.Entity, entry.Slots)
	}
	entry.Terms = collectTerms(entry)
	return entry
}

func collectTerms(entry Entry) []string {
	set := map[string]struct{}{}
	addTokens := func(parts ...string) {
		for _, part := range parts {
			for _, token := range tokenize(part) {
				set[token] = struct{}{}
			}
		}
	}

	addTokens(entry.Text, entry.Entity, entry.Source)
	for _, key := range sortedKeys(entry.Slots) {
		addTokens(key, entry.Slots[key], key+" "+entry.Slots[key])
	}
	for _, key := range sortedKeys(entry.Tags) {
		addTokens(key, entry.Tags[key], key+" "+entry.Tags[key])
	}

	terms := make([]string, 0, len(set))
	for term := range set {
		terms = append(terms, term)
	}
	sort.Strings(terms)
	return terms
}

type parsedQuery struct {
	Tokens      []string
	Facets      []string
	ExactEntity string
}

func parseQuery(query string) parsedQuery {
	parts := strings.Fields(strings.TrimSpace(query))
	facets := make([]string, 0, len(parts))
	tokenSet := map[string]struct{}{}
	for _, part := range parts {
		if facet := normalizeFacetTerm(part); facet != "" {
			facets = append(facets, facet)
			continue
		}
		for _, token := range tokenize(part) {
			tokenSet[token] = struct{}{}
		}
	}

	tokens := make([]string, 0, len(tokenSet))
	for token := range tokenSet {
		tokens = append(tokens, token)
	}
	sort.Strings(tokens)

	return parsedQuery{
		Tokens:      tokens,
		Facets:      facets,
		ExactEntity: normalizePhrase(query),
	}
}

func normalizeFacetTerm(raw string) string {
	if !strings.Contains(raw, "=") {
		return ""
	}
	parts := strings.SplitN(raw, "=", 2)
	key := normalizePhrase(parts[0])
	value := normalizePhrase(parts[1])
	if key == "" || value == "" {
		return ""
	}

	switch {
	case key == "entity":
		return "entity=" + value
	case key == "day":
		return "day=" + value
	case strings.HasPrefix(key, "slot."):
		return key + "=" + value
	case strings.HasPrefix(key, "tag."):
		return key + "=" + value
	default:
		return key + "=" + value
	}
}

func tokenize(s string) []string {
	var tokens []string
	var current strings.Builder
	for _, r := range strings.ToLower(s) {
		if unicode.IsLetter(r) || unicode.IsNumber(r) {
			current.WriteRune(r)
			continue
		}
		if current.Len() > 0 {
			token := current.String()
			if len(token) > 1 || unicode.IsNumber(rune(token[0])) {
				tokens = append(tokens, token)
			}
			current.Reset()
		}
	}
	if current.Len() > 0 {
		token := current.String()
		if len(token) > 1 || unicode.IsNumber(rune(token[0])) {
			tokens = append(tokens, token)
		}
	}
	return tokens
}

func normalizePhrase(s string) string {
	return strings.Join(tokenize(s), " ")
}

func syntheticText(entity string, slots map[string]string) string {
	keys := sortedKeys(slots)
	parts := make([]string, 0, len(keys))
	for _, key := range keys {
		value := strings.TrimSpace(slots[key])
		if value == "" {
			continue
		}
		parts = append(parts, fmt.Sprintf("%s=%s", key, value))
	}
	return strings.TrimSpace(entity + " " + strings.Join(parts, " "))
}

func preview(text string) string {
	clean := strings.Join(strings.Fields(text), " ")
	if len(clean) <= 140 {
		return clean
	}
	return clean[:137] + "..."
}

// effectiveTrust returns the trust value to use in scoring.
// A zero value means the field was absent (old entry) and defaults to 1.0.
func effectiveTrust(t float64) float64 {
	if t == 0 {
		return 1.0
	}
	return t
}

// jaccardSim computes Jaccard similarity between a query token set and an
// entry's term list: |intersection| / |union|. Returns 0 when both are empty.
func jaccardSim(queryTokens map[string]struct{}, entryTerms []string) float64 {
	if len(queryTokens) == 0 && len(entryTerms) == 0 {
		return 0
	}

	// Build entry term set.
	entrySet := make(map[string]struct{}, len(entryTerms))
	for _, t := range entryTerms {
		entrySet[t] = struct{}{}
	}

	// Count intersection.
	intersection := 0
	for t := range queryTokens {
		if _, ok := entrySet[t]; ok {
			intersection++
		}
	}

	// Union = |A| + |B| - |A∩B|
	union := len(queryTokens) + len(entrySet) - intersection
	if union == 0 {
		return 0
	}
	return float64(intersection) / float64(union)
}

// temporalDecay returns 0.5^(age_days / halfLifeDays).
// A freshly created entry returns 1.0; an entry exactly halfLifeDays old
// returns 0.5; older entries decay exponentially toward 0.
func temporalDecay(createdAt time.Time, halfLifeDays float64, now time.Time) float64 {
	ageDays := now.Sub(createdAt).Hours() / 24
	if ageDays < 0 {
		ageDays = 0
	}
	return math.Pow(0.5, ageDays/halfLifeDays)
}

func appendID(ids []uint64, id uint64) []uint64 {
	if n := len(ids); n > 0 && ids[n-1] == id {
		return ids
	}
	return append(ids, id)
}

func intersectIDs(left, right []uint64) []uint64 {
	result := make([]uint64, 0, min(len(left), len(right)))
	i, j := 0, 0
	for i < len(left) && j < len(right) {
		switch {
		case left[i] == right[j]:
			result = append(result, left[i])
			i++
			j++
		case left[i] < right[j]:
			i++
		default:
			j++
		}
	}
	return result
}

func cloneIDs(ids []uint64) []uint64 {
	if len(ids) == 0 {
		return nil
	}
	out := make([]uint64, len(ids))
	copy(out, ids)
	return out
}

func cloneMap(in map[string]string) map[string]string {
	if len(in) == 0 {
		return nil
	}
	out := make(map[string]string, len(in))
	for key, value := range in {
		out[key] = value
	}
	return out
}

func sortedKeys(in map[string]string) []string {
	if len(in) == 0 {
		return nil
	}
	keys := make([]string, 0, len(in))
	for key := range in {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	return keys
}

func maxID(a, b uint64) uint64 {
	if a > b {
		return a
	}
	return b
}

func min(a, b int) int {
	if a < b {
		return a
	}
	return b
}
