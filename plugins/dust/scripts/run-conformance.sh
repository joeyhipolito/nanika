#!/usr/bin/env bash
# run-conformance.sh — Build dust-conform + fixture, then run the conformance suite.
#
# Usage:
#   plugins/dust/scripts/run-conformance.sh [--verbose] [--section <name>]
#
# Options:
#   --verbose     Print each section name and result as it runs.
#   --section N   Run only the named section (handshake|methods|heartbeat|shutdown).
#   --no-build    Skip cargo build (useful when binaries are already current).
#
# Exit codes:
#   0  all sections passed
#   1  one or more sections failed
#   2  build failed

set -euo pipefail

# ── Locate the workspace root ──────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# ── Parse flags ───────────────────────────────────────────────────────────────
VERBOSE=""
SECTION_ARG=""
SKIP_BUILD=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --verbose|-v)   VERBOSE="--verbose";   shift ;;
        --section)      SECTION_ARG="--section $2"; shift 2 ;;
        --no-build)     SKIP_BUILD=1;           shift ;;
        *)              echo "unknown flag: $1" >&2; exit 2 ;;
    esac
done

# ── Build ─────────────────────────────────────────────────────────────────────
if [[ ${SKIP_BUILD} -eq 0 ]]; then
    echo "[build] cargo build -p dust-conformance -p dust-fixture-minimal ..."
    (
        cd "${WORKSPACE_ROOT}"
        cargo build -p dust-conformance -p dust-fixture-minimal 2>&1
    ) || { echo "[build] FAILED" >&2; exit 2; }
    echo "[build] OK"
fi

# ── Locate binaries ───────────────────────────────────────────────────────────
CONFORM_BIN="${WORKSPACE_ROOT}/target/debug/dust-conform"
MANIFEST="${WORKSPACE_ROOT}/dust-conformance/fixtures/minimal/plugin.json"

if [[ ! -x "${CONFORM_BIN}" ]]; then
    echo "[error] dust-conform binary not found at ${CONFORM_BIN}" >&2
    exit 2
fi
if [[ ! -f "${MANIFEST}" ]]; then
    echo "[error] plugin.json not found at ${MANIFEST}" >&2
    exit 2
fi

# ── Run conformance suite ──────────────────────────────────────────────────────
echo "[conform] running dust-conform --plugin-manifest ${MANIFEST} --json ${VERBOSE} ${SECTION_ARG}"
RESULTS=$(
    "${CONFORM_BIN}" \
        --plugin-manifest "${MANIFEST}" \
        --json \
        ${VERBOSE} \
        ${SECTION_ARG}
)

echo "${RESULTS}"

# ── Check results ─────────────────────────────────────────────────────────────
FAILED=$(echo "${RESULTS}" | python3 -c "
import json, sys
data = json.load(sys.stdin)
failed = [r for r in data if not r.get('passed')]
for r in failed:
    print(f'  FAIL {r[\"section\"]}: {r[\"message\"]}')
sys.exit(1 if failed else 0)
" 2>&1) || {
    echo "[conform] FAILED sections:" >&2
    echo "${FAILED}" >&2
    exit 1
}

TOTAL=$(echo "${RESULTS}" | python3 -c "import json,sys; print(len(json.load(sys.stdin)))" 2>/dev/null || echo "?")
echo "[conform] all ${TOTAL} sections passed"
exit 0
