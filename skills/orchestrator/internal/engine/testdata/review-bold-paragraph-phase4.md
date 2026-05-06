I have the full spec now. Let me run the success checks systematically.I have enough to produce the review. Let me write the verification report.Review complete — report written to `output.md`.

**Summary:** 2 blockers, 5 warnings, 2 suggestions.

**Blockers:**
1. `~/dev/lab/warmer/scripts/send-opener.ts` is **untracked** — phase-1's required commit never landed. Most recent warmer commit is `07f2234 phase review:…`, not the expected `feat(scripts): add send-opener…`.
2. Plugin test `test/restricted-scope.test.ts:240` fails with `TypeError: deps.runOpener is not a function` — the pre-existing test was not updated when the stub was replaced. `bun test` in the plugin reports 29 pass / 1 fail.

**Warnings (spec-wording drift in `send-opener.ts`):** guardrail stderr missing `refused —` prefix and `(use --force to override)` hint; dry-run stdout missing ` — guardrail OK` suffix; "invalid phone number" vs spec's "invalid phone format"; "lead not found for X" vs spec's "no lead found with phone X"; null-name fallback not implemented.

**What's right:** plugin wiring is clean and surgical (`scope.ts` + `server.ts` only, `acquireSingletonLock` untouched, `runWarmerOpener` structurally identical to `runWarmerReset`); both `tsc --noEmit` pass; Thenna's record is undisturbed (`COLD|[]`); DB shasum stable across all dry-runs.

**Mission-text issue:** verify step called for `status='WARM'` but `poc.db` CHECK constraint forbids WARM — I substituted `CONTACTED` to still exercise the non-COLD branch.

