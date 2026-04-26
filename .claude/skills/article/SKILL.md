---
name: article
description: Full article pipeline — from topic research to Substack draft. Scouts trending intel, writes or selects an article, generates intaglio art via Google Flow, creates manifest, and cross-posts to Substack. Use when creating, illustrating, or publishing articles.
disable-model-invocation: true
allowed-tools: Bash Read Write Edit Grep Glob Agent
---

# Article Pipeline

End-to-end pipeline for producing and publishing illustrated articles.

## Paths

| What | Path |
|------|------|
| Personal website | `~/dev/personal/joeyhipolito.dev/` |
| Article content | `~/dev/personal/joeyhipolito.dev/content/{log,newsletter}/` |
| Article images | `~/dev/personal/joeyhipolito.dev/public/{log,newsletter}/<slug>/` |
| Intaglio style bible | `${CLAUDE_SKILL_DIR}/intaglio-bible.md` |
| Flow generate script | `${CLAUDE_SKILL_DIR}/scripts/flow-generate.sh` |
| Mascot reference | `~/.contentkit/references/mascot.png` |
| ContentKit styles | `~/.contentkit/styles/intaglio/` |
| Engage queue | `~/.alluka/engage/queue/` |

## Content Types

| Kind | Directory | Description |
|------|-----------|-------------|
| `log` | `content/log/` | Technical deep-dives, system architecture, builder narratives |
| `newsletter` | `content/newsletter/` | Commentary, opinion, industry analysis |

## Pipeline Steps

### Step 1: Scout for trending topics

```bash
scout gather --since 24h
scout intel "ai-coding" --since 24h
```

Browse topics, find high-engagement threads. Pick a topic — or use an existing draft article.

### Step 2: Research the topic

Before writing, deeply research the chosen topic:
- Read the source thread/post (use `agent-browser` or `WebFetch` to get full content)
- Find related threads, papers, or data (check scout intel for cross-references)
- Identify the specific numbers, quotes, and evidence that make this worth writing about
- Find the angle that connects to nanika/the author's own experience building AI agent systems

### Step 3: Craft the title

**The title comes first. The article is built to address it.**

The title makes a provocative, slightly dishonest claim. The article then defends, qualifies, or dismantles that claim with evidence. The reader clicks because the title stings. They stay because the article is honest.

**Title rules:**
- Make a bold claim that feels slightly wrong or uncomfortable
- Short — under 8 words if possible
- Use "you" or "your" to make it personal
- State a conclusion, not a question (questions are weaker hooks)
- The claim doesn't need to be literally true — but the article MUST address why it's true, partially true, or interestingly wrong
- Study these examples from the author's LinkedIn (3,000-4,000 impressions each):
  - "AI chose nukes. Every time." — article reveals the 95% number has problems but the zero de-escalation doesn't
  - "Your CLAUDE.md Is Making Things Worse" — article shows when it's true and the 3 cases where it isn't
  - "The Death of Code Review" — article explains why the economics are breaking, not that review is literally dead
  - "Companies are only hiring vibe coders now." — article unpacks what the recruiter actually meant

### Step 4: Write the article (MDX)

Write the full article at `~/dev/personal/joeyhipolito.dev/content/{log,newsletter}/<date>-<slug>.mdx`

Read `${CLAUDE_SKILL_DIR}/voice.md` for the full voice reference. Key rules for articles:

**Voice:**
- Write like explaining something to a friend who codes. Not a conference talk, not a blog post.
- Simple words. If you wouldn't say it out loud to a coworker, don't write it.
- Be honest but fair. When something is overhyped, say so. Acknowledge what's good too.
- When someone is wrong, don't say they're wrong. Say "I tried something similar and found that..."
- No exclamation marks. No performed excitement. No hedging with stacked qualifiers.
- Contractions always. Short sentences. One idea at a time.

**Banned words:** convergence, leverage, utilize, framework (when you mean "tool"), ecosystem, robust, scalable, groundbreaking, game-changer, paradigm, innovative, cutting-edge, transformative, arguably, it's worth noting, interestingly

**Structure — title-first approach:**
- Open by confronting the title's claim within the first 2-3 paragraphs. The reader clicked because of the title — pay it off early.
- Use a specific number, finding, or event as the opening evidence for or against the claim
- 1500-3000 words. 4-7 sections with H2 headings.
- Each section should have concrete evidence: numbers, quotes, commit counts, error logs, specific tool names
- Connect everything back to what the author has actually built and run with nanika
- By the end, the reader should understand exactly how the title is true, partially true, or usefully wrong
- End with an honest open question, not a conclusion that wraps things up neatly
- Place `<Figure>` tags at section transitions (max 3-4 total, not after every heading)

**Content types:**
- `log` — technical deep-dives, builder narratives, system architecture (written from experience building nanika)
- `newsletter` — commentary, opinion, industry analysis (written from watching the industry while building)

Frontmatter:
```yaml
---
id: <slug>
title: "Article Title"
date: "YYYY-MM-DD"
description: "Short subtitle (under 256 chars — Substack rejects longer)"
published: false
tags: ["ai", "agents", "topic"]
author: "Jose Marcelius Hipolito"
excerpt: "Reader-facing excerpt"
kind: log  # or newsletter
---
```

### Step 5: Generate art prompts

Read the full article. For each image needed (1 cover + 1 hero + 0-3 inline), create a prompt directory:

```
/tmp/illustrate/<slug>/
├── 000-cover/
│   ├── prompt.md
│   └── mascot.png  (only if nanika appears)
├── 001-hero/
│   ├── prompt.md
│   └── mascot.png  (only if nanika appears)
└── 002-section-name/
    └── prompt.md
```

**CRITICAL: Every prompt.md must start with `**Illustration:** <folder-name>`** — this tag is how the download step maps generated images back to folders.

#### Prompt format

Read `${CLAUDE_SKILL_DIR}/intaglio-bible.md` for the full style rules. Key points:

- **Intaglio engraving on cream paper `#F2EAD7`**
- **One terracotta `#DA7757` object per image** — the focal point
- **Literal/editorial scenes, NOT metaphors**
- **Cover image (000-cover):** 1200×630, more expressive, focal point centered in safe 60% zone
- **Hero/inline:** 950×500 landscape

When nanika appears, describe rendering explicitly:
```
Mask face is smooth bare cream paper framed by dense crosshatched black hair.
Circuit-board robe in fine geometric hatching. Headband with emotion faces
in delicate line work. White gloves are bare paper with faint contour lines.
Monochrome. Match reference image proportions — chibi with oversized head.
```

Do NOT just say "chibi" — describe how each feature translates to engraving.

Nanika is optional — use judgment based on article context. Default to no mascot.

Copy `~/.contentkit/references/mascot.png` into any prompt folder where nanika appears.

### Step 6: Generate images (Google Flow)

```bash
${CLAUDE_SKILL_DIR}/scripts/flow-generate.sh <slug> <log|newsletter> [--style intaglio]
```

This script:
1. Creates or reuses a Flow project
2. Dismisses any consent modals
3. For each prompt folder: attaches mascot reference (if present), pastes prompt, clicks Create
4. Waits for all images to generate
5. Downloads and places images in `~/dev/personal/joeyhipolito.dev/public/<type>/<slug>/`

**Requires:** Chrome debug running on port 9222 (`--remote-debugging-port=9222 --user-data-dir=~/.chrome-debug`).

### Step 7: Create manifest

Create `~/dev/personal/joeyhipolito.dev/public/<type>/<slug>/manifest.json`:

```json
{
  "article": "<full-path-to-mdx>",
  "created": "<ISO-8601>",
  "cover": "<full-path-to-cover.png>",
  "assets": [
    {
      "type": "illustration",
      "section": "",
      "description": "Hero alt text",
      "path": "<full-path-to-hero.png>"
    },
    {
      "type": "illustration",
      "section": "Section Heading",
      "description": "Inline alt text",
      "path": "<full-path-to-inline.png>"
    }
  ]
}
```

### Step 8: Update Figure tags in MDX

Ensure each `<Figure>` in the article has the correct `src` path and descriptive `alt` text:

```mdx
<Figure src="/<type>/<slug>/hero.png" alt="Descriptive alt text" />
```

### Step 9: Cross-post to Substack

```bash
substack draft <article.mdx> \
  --manifest <manifest.json> \
  --public-dir ~/dev/personal/joeyhipolito.dev/public \
  --tags "tag1,tag2" \
  --audience everyone
```

**Note:** Description/subtitle must be under 256 characters or Substack returns HTTP 400.

Verify: `substack drafts` — should show the new draft.

### Step 10 (optional): Cross-platform adapt

```bash
engage adapt --platforms linkedin,x,reddit <article.mdx>
engage review
engage approve <id>
engage post
```
