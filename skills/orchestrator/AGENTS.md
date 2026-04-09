# AGENTS.md

## Direct Review Protocol

When the user asks for a direct review, PR review, or "look up the change and review it":

- Resolve the exact review target first: issue ID, PR number, branch, base/head SHAs, or the specific commit range. Do not review the current working tree by default if the request is about a discrete change.
- Review the clean diff, not unrelated local edits. If the checkout is dirty, anchor the review to the exact commit range or use a detached worktree so unrelated modifications do not contaminate findings.
- Treat GitHub PR review as diff-centric. If Codex is reviewing in GitHub, match comments to the actual PR diff and merged context, not to later local changes.
- Findings come first and are ordered by severity. Use file/line references when the problem is code-level. Keep summaries brief and secondary.
- Review for real behavior, not just intent:
  - confirm the change actually does what the issue or PR says it does
  - verify entrypoints and call sites, not only leaf helpers
  - check fresh-run, resume, retry, rollback, degraded, and missing-dependency paths when they are relevant
  - for test-only changes, verify the tests prove the production path rather than re-testing helper internals
- Distinguish levels of review:
  - code-level bugs, regressions, unsafe assumptions, and missing assertions are findings
  - systems-level gaps include unverified boundaries, silent failure paths, missing observability, and rollout or rollback risks
- If no material issues are found, say that explicitly and note any residual risk or testing gap instead of giving a drive-by approval.
- Verification is part of review. Run targeted tests or checks when feasible, and state exactly what was or was not verified.
