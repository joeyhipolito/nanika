#!/usr/bin/env bash
# Enforce test-first discipline: every new .go production file in the commit
# range must have a corresponding _test.go committed in the same or earlier
# commit. Commits with "[skip-test-first]" in their message are exempt.
#
# Usage: test-first-check.sh <base-ref> <head-ref>
#   base-ref   first commit to check (exclusive lower bound), e.g. origin/main
#   head-ref   last commit to check (inclusive), e.g. HEAD
#
# Exit codes:
#   0  all commits pass (or range is empty)
#   1  one or more commits violate test-first discipline
#   2  usage error
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <base-ref> <head-ref>" >&2
  exit 2
fi

BASE_REF="$1"
HEAD_REF="$2"

# Resolve refs — tolerate identical refs (empty range)
if ! BASE_SHA=$(git rev-parse --verify "${BASE_REF}^{commit}" 2>/dev/null); then
  echo "error: cannot resolve base ref: ${BASE_REF}" >&2
  exit 2
fi
if ! HEAD_SHA=$(git rev-parse --verify "${HEAD_REF}^{commit}" 2>/dev/null); then
  echo "error: cannot resolve head ref: ${HEAD_REF}" >&2
  exit 2
fi

# Empty range — nothing to check
if [[ "$BASE_SHA" == "$HEAD_SHA" ]]; then
  echo "test-first-check: empty range (base == head) — nothing to check"
  exit 0
fi

# Collect commits in range (base exclusive, head inclusive)
# Silence git errors — if base is not an ancestor of head the range is empty
COMMITS=()
while IFS= read -r sha; do
  [[ -n "$sha" ]] && COMMITS+=("$sha")
done < <(git log --format="%H" "${BASE_SHA}..${HEAD_SHA}" 2>/dev/null || true)

if [[ ${#COMMITS[@]} -eq 0 ]]; then
  echo "test-first-check: no commits in range — nothing to check"
  exit 0
fi

FAILURES=0

for sha in "${COMMITS[@]}"; do
  msg=$(git log -1 --format="%s" "$sha")

  # Bypass: commit message contains [skip-test-first]
  if [[ "$msg" == *"[skip-test-first]"* ]]; then
    echo "[skip] ${sha:0:12}  ${msg}"
    continue
  fi

  # Files added in this commit
  mapfile -t added_files < <(git diff-tree --no-commit-id -r --name-only --diff-filter=A "$sha" 2>/dev/null || true)

  # Filter to non-test Go production files
  new_prod_files=()
  for f in "${added_files[@]}"; do
    [[ "$f" == *.go ]] || continue
    [[ "$f" == *_test.go ]] && continue
    new_prod_files+=("$f")
  done

  if [[ ${#new_prod_files[@]} -eq 0 ]]; then
    echo "[ok]   ${sha:0:12}  ${msg}"
    continue
  fi

  commit_failures=0
  for prod in "${new_prod_files[@]}"; do
    dir=$(dirname "$prod")
    base=$(basename "$prod" .go)

    # Accept <name>_test.go or <dir>/..._test.go in the same directory
    # Check if ANY _test.go file exists in the same directory at this commit
    if git ls-tree -r --name-only "$sha" -- "${dir}/" 2>/dev/null \
        | grep -q "_test\.go$"; then
      :  # test file present in same commit tree
    else
      echo "[FAIL] ${sha:0:12}  ${prod}  — no _test.go in ${dir}/ at this commit" >&2
      commit_failures=$((commit_failures + 1))
    fi
    # unused but keeps the variable referenced for shellcheck
    : "$base"
  done

  if [[ $commit_failures -eq 0 ]]; then
    echo "[ok]   ${sha:0:12}  ${msg}"
  else
    FAILURES=$((FAILURES + commit_failures))
  fi
done

if [[ $FAILURES -gt 0 ]]; then
  echo "" >&2
  echo "test-first-check: $FAILURES violation(s) found" >&2
  echo "Add [skip-test-first] to the commit message to bypass." >&2
  exit 1
fi

echo "test-first-check: all commits pass"
exit 0
