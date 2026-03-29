package routing

import "testing"

// TestClassifyTaskType verifies the full priority order and keyword boundary
// behavior of ClassifyTaskType. Each case is crafted to expose a specific
// rule interaction — especially keywords that appear in multiple rules.
func TestClassifyTaskType(t *testing.T) {
	cases := []struct {
		task string
		want TaskType
	}{
		// ── Empty input ────────────────────────────────────────────────────────
		{"", TaskTypeUnknown},

		// ── Bugfix wins over everything ────────────────────────────────────────
		// "fix " with trailing space is the bugfix trigger.
		// GOTCHA: "suffix " contains "fix " as a substring (suf + fix + space),
		// so any task with "suffix " also fires bugfix. Design the keyword rule
		// with this in mind: it is intentional that "fix " is broad.
		{"fix the login crash", TaskTypeBugfix},
		{"add a suffix fix", TaskTypeBugfix}, // "suffix " contains "fix "
		{"debug the nil pointer", TaskTypeBugfix},
		{"repair broken auth", TaskTypeBugfix},
		{"the build is failing", TaskTypeBugfix},
		// " error" with leading space catches "the error", "an error", and also
		// " errors" (plural) — any task mentioning "errors" is classified bugfix.
		{"investigate the error", TaskTypeBugfix},
		{"there are errors in the config", TaskTypeBugfix},

		// ── Deployment ─────────────────────────────────────────────────────────
		{"deploy the service to prod", TaskTypeDeployment},
		{"rollout the new version", TaskTypeDeployment},
		{"run the database migration", TaskTypeDeployment},
		// "release " with trailing space avoids matching "press release" → still deployment
		{"release the v2.0 binary", TaskTypeDeployment},

		// ── Test ───────────────────────────────────────────────────────────────
		{"write unit tests for the parser", TaskTypeTest},
		{"add test coverage for edge cases", TaskTypeTest},
		{"run the benchmark suite", TaskTypeTest},
		{"fuzz the HTTP handler", TaskTypeTest},
		// "testing" beats implementation's "write" even when both appear
		{"write testing fixtures", TaskTypeTest},

		// ── Refactor ──────────────────────────────────────────────────────────
		{"refactor the auth middleware", TaskTypeRefactor},
		{"clean up the routing package", TaskTypeRefactor},
		{"cleanup dead code in cmd/", TaskTypeRefactor},
		{"restructure the config system", TaskTypeRefactor},
		// "simplify" is a refactor keyword; avoid "error" in this task because
		// " error" (bugfix keyword) would fire first on "simplify the error handling".
		{"simplify the routing logic", TaskTypeRefactor},

		// ── Docs ──────────────────────────────────────────────────────────────
		{"write documentation for the API", TaskTypeDocs},
		{"update the readme", TaskTypeDocs},
		{"create a design doc for auth", TaskTypeDocs},
		// " spec " with surrounding spaces avoids "specialize"
		{"write a spec for the new feature", TaskTypeDocs},

		// ── Writing (must beat implementation's broad "write" keyword) ─────────
		// Avoid using "error(s)" in writing tasks: " error" is a bugfix keyword
		// that fires first regardless of other keywords in the same sentence.
		{"write a blog post about Go concurrency", TaskTypeWriting},
		{"write blog post on dependency injection", TaskTypeWriting},
		{"draft an article about CLI design", TaskTypeWriting},
		{"draft blog on SQLite patterns", TaskTypeWriting},
		{"draft post for the company update", TaskTypeWriting},
		{"create a newsletter for subscribers", TaskTypeWriting},
		{"publish a substack note", TaskTypeWriting},
		{"write a linkedin post", TaskTypeWriting},
		{"create a reddit post for r/golang", TaskTypeWriting},
		{"publish post to Substack", TaskTypeWriting},
		{"record narration for the video", TaskTypeWriting},
		{"content creation pipeline", TaskTypeWriting},

		// ── Research (broad keywords, must not eat implementation tasks) ────────
		// Avoid "error(s)" in research tasks: " error" (bugfix) fires first.
		{"research Go caching patterns", TaskTypeResearch},
		{"investigate the memory leak", TaskTypeResearch},
		{"audit the authentication flow", TaskTypeResearch},
		{"explore caching strategies", TaskTypeResearch},
		{"review the pull request", TaskTypeResearch},
		{"study the SQLite WAL mode", TaskTypeResearch},

		// ── Implementation (catch-all) ─────────────────────────────────────────
		{"implement the retry logic", TaskTypeImplementation},
		{"add pagination to the API", TaskTypeImplementation},
		{"build the CLI scaffolding", TaskTypeImplementation},
		{"create the user store", TaskTypeImplementation},
		{"develop the scoring algorithm", TaskTypeImplementation},
		// "write" alone → implementation (not writing, because no content keyword)
		{"write the HTTP handler", TaskTypeImplementation},
		// " code" with leading space
		{"produce code for the exporter", TaskTypeImplementation},

		// ── Unknown ───────────────────────────────────────────────────────────
		{"do the thing", TaskTypeUnknown},
		{"update the spreadsheet", TaskTypeUnknown},
	}

	for _, tc := range cases {
		got := ClassifyTaskType(tc.task)
		if got != tc.want {
			t.Errorf("ClassifyTaskType(%q) = %q, want %q", tc.task, got, tc.want)
		}
	}
}

// TestClassifyTaskType_CaseFolding verifies that classification is
// case-insensitive and treats uppercase task text the same as lowercase.
func TestClassifyTaskType_CaseFolding(t *testing.T) {
	cases := []struct {
		task string
		want TaskType
	}{
		{"FIX the login bug", TaskTypeBugfix},
		{"WRITE BLOG POST about Go", TaskTypeWriting},
		{"IMPLEMENT the retry logic", TaskTypeImplementation},
		{"RESEARCH caching strategies", TaskTypeResearch},
		{"Deploy to production", TaskTypeDeployment},
	}
	for _, tc := range cases {
		got := ClassifyTaskType(tc.task)
		if got != tc.want {
			t.Errorf("ClassifyTaskType(%q) = %q, want %q", tc.task, got, tc.want)
		}
	}
}

// TestClassifyTaskType_PriorityOrder verifies that higher-priority rules
// win when a task matches multiple rule keywords simultaneously.
func TestClassifyTaskType_PriorityOrder(t *testing.T) {
	cases := []struct {
		task string
		want TaskType
		why  string
	}{
		// bugfix wins over deployment ("fix " beats "migrate")
		{"fix the migration script", TaskTypeBugfix, "bugfix > deployment"},
		// bugfix wins over implementation: "fix " beats "add "
		{"fix and add the new feature", TaskTypeBugfix, "bugfix > implementation"},
		// test wins over implementation ("test" beats "write")
		{"write and test the parser", TaskTypeTest, "test > implementation"},
		// refactor wins over research ("refactor" beats "review")
		{"review and refactor the auth package", TaskTypeRefactor, "refactor > research"},
		// docs wins over implementation ("readme" beats "create")
		{"create a readme for the service", TaskTypeDocs, "docs > implementation"},
		// writing wins over implementation ("blog post" beats "write")
		{"write a blog post about my implementation", TaskTypeWriting, "writing > implementation"},
		// writing wins over research ("newsletter" beats "review")
		{"review the newsletter draft", TaskTypeWriting, "writing > research"},
	}
	for _, tc := range cases {
		got := ClassifyTaskType(tc.task)
		if got != tc.want {
			t.Errorf("ClassifyTaskType(%q) = %q, want %q (%s)", tc.task, got, tc.want, tc.why)
		}
	}
}
