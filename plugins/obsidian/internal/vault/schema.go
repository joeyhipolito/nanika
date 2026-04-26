package vault

// Frontmatter type values — kind-agnostic. Both vault kinds emit the same
// type: vocabulary; these constants exist for grep-ability.
const (
	TypeFleeting = "fleeting"
	TypeDaily    = "daily"
	TypeIdea     = "idea"
	TypeMission  = "mission"
	TypeNote     = "note"
	TypeMOC      = "moc"
	TypeTopic    = "topic"
)

// Nanika folder constants. Every nanika vault path segment used anywhere in
// the plugin must resolve to one of these.
const (
	NanikaInbox     = "inbox"
	NanikaIdeas     = "ideas"
	NanikaMissions  = "missions"
	NanikaDaily     = "daily"
	NanikaMOCs      = "mocs"
	NanikaTrackers  = "trackers"
	NanikaSessions  = "sessions"
	NanikaFindings  = "findings"
	NanikaDecisions = "decisions"
	NanikaQuestions = "questions"
)

// SecondBrain folder constants.
const (
	SecondBrainInbox     = "inbox"
	SecondBrainIdeas     = "ideas"
	SecondBrainDaily     = "daily"
	SecondBrainMOCs      = "mocs"
	SecondBrainFindings  = "findings"
	SecondBrainDecisions = "decisions"
	SecondBrainQuestions = "questions"
	SecondBrainTopics    = "topics"
)

// IndexFile is the root entry-point note written by InitSkeleton.
const IndexFile = "index.md"

// Schema describes the folder layout of one vault kind. Fields hold the
// folder names callers should filepath.Join onto the vault root. Optional
// folders absent from a given kind are the empty string.
type Schema struct {
	Kind VaultKind

	// Core folders present in every kind.
	Inbox     string
	Ideas     string
	Daily     string
	MOCs      string
	Findings  string
	Decisions string
	Questions string

	// Kind-specific folders — empty string when not applicable.
	Missions string // nanika only
	Trackers string // nanika only
	Sessions string // nanika only
	Topics   string // second-brain only

	// Dirs is the ordered list of folders that InitSkeleton creates.
	Dirs []string

	// ScanRoots is the ordered list of folders MOC detection scans.
	// Excludes MOCs itself and Inbox (neither is a zettel source).
	ScanRoots []string

	// MOCTemplate is the index.md body written on skeleton init.
	MOCTemplate string
}

const nanikaMOCTemplate = `---
type: moc
status: active
title: Nanika Vault Entry
---
# Nanika Vault Entry

## Active ideas

## Recent missions

## Today

## Scratch
`

const secondBrainMOCTemplate = `---
type: moc
status: active
title: Second Brain Entry
---
# Second Brain Entry

## Active ideas

## Today

## Scratch
`

// NanikaSchema is the canonical layout for the nanika agent vault.
var NanikaSchema = Schema{
	Kind:      KindNanika,
	Inbox:     NanikaInbox,
	Ideas:     NanikaIdeas,
	Missions:  NanikaMissions,
	Daily:     NanikaDaily,
	MOCs:      NanikaMOCs,
	Trackers:  NanikaTrackers,
	Sessions:  NanikaSessions,
	Findings:  NanikaFindings,
	Decisions: NanikaDecisions,
	Questions: NanikaQuestions,
	Dirs: []string{
		NanikaInbox, NanikaIdeas, NanikaMissions, NanikaDaily, NanikaMOCs,
		NanikaTrackers, NanikaSessions, NanikaFindings, NanikaDecisions, NanikaQuestions,
	},
	ScanRoots: []string{
		NanikaMissions, NanikaDaily, NanikaSessions, NanikaIdeas,
	},
	MOCTemplate: nanikaMOCTemplate,
}

// SecondBrainSchema is the canonical layout for the personal knowledge vault.
var SecondBrainSchema = Schema{
	Kind:      KindSecondBrain,
	Inbox:     SecondBrainInbox,
	Ideas:     SecondBrainIdeas,
	Daily:     SecondBrainDaily,
	MOCs:      SecondBrainMOCs,
	Findings:  SecondBrainFindings,
	Decisions: SecondBrainDecisions,
	Questions: SecondBrainQuestions,
	Topics:    SecondBrainTopics,
	Dirs: []string{
		SecondBrainInbox, SecondBrainIdeas, SecondBrainDaily, SecondBrainMOCs,
		SecondBrainFindings, SecondBrainDecisions, SecondBrainQuestions, SecondBrainTopics,
	},
	ScanRoots: []string{
		SecondBrainDaily, SecondBrainIdeas,
	},
	MOCTemplate: secondBrainMOCTemplate,
}

// SchemaFor returns the schema for a given vault kind. Unknown kinds fall
// back to NanikaSchema for backward compatibility.
func SchemaFor(kind VaultKind) Schema {
	if kind == KindSecondBrain {
		return SecondBrainSchema
	}
	return NanikaSchema
}
