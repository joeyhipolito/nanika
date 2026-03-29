#!/usr/bin/env bash
set -euo pipefail

SUITES=(decomposer persona-routing review-findings)
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
EVALS_DIR="${SCRIPT_DIR}/../evals"

usage() {
  echo "Usage: $0 [decomposer|persona-routing|review-findings|all]" >&2
  echo "  Defaults to 'decomposer' when no argument is given." >&2
  exit 1
}

ARG="${1:-decomposer}"

case "$ARG" in
  all)
    RUN_SUITES=("${SUITES[@]}")
    ;;
  decomposer|persona-routing|review-findings)
    RUN_SUITES=("$ARG")
    ;;
  -h|--help)
    usage
    ;;
  *)
    echo "Unknown suite: $ARG" >&2
    usage
    ;;
esac

passed=0
failed=0
skipped=0

for suite in "${RUN_SUITES[@]}"; do
  config="$EVALS_DIR/${suite}.yaml"
  if [[ ! -f "$config" ]]; then
    echo "warning: skipping suite '$suite' — $config not found"
    (( skipped++ )) || true
    continue
  fi

  echo "==> Running suite: $suite"
  if npx promptfoo eval -c "$config"; then
    (( passed++ )) || true
  else
    echo "error: suite '$suite' failed"
    (( failed++ )) || true
  fi
done

total=$(( passed + failed + skipped ))
echo ""
echo "Summary: ${total} suite(s) — ${passed} passed, ${failed} failed, ${skipped} skipped"

if (( failed > 0 )); then
  exit 1
fi
