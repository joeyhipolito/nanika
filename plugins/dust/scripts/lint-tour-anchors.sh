#!/usr/bin/env bash
# lint-tour-anchors.sh — Verify every spotlightTarget anchor referenced from
# src/whim/tour/steps.ts has at least one matching `data-tour-anchor` declaration
# under src/whim/** (covers static `data-tour-anchor="X"` attributes and dynamic
# JSX expressions like `data-tour-anchor={cond ? 'X' : undefined}`).
#
# Exit codes:
#   0  every spotlight anchor resolves to at least one declaration
#   1  drift detected (anchors referenced but never declared)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

STEPS_FILE="${PLUGIN_ROOT}/src/whim/tour/steps.ts"
WHIM_DIR="${PLUGIN_ROOT}/src/whim"

if [[ ! -f "${STEPS_FILE}" ]]; then
    echo "lint:tour: missing ${STEPS_FILE}" >&2
    exit 1
fi
if [[ ! -d "${WHIM_DIR}" ]]; then
    echo "lint:tour: missing ${WHIM_DIR}" >&2
    exit 1
fi

# Anchor names referenced from steps.ts via spotlightTarget: "[data-tour-anchor='X']".
referenced=$(grep -Eo "data-tour-anchor='[^']+'" "${STEPS_FILE}" \
    | sed -E "s/data-tour-anchor='([^']+)'/\1/" \
    | sort -u)

# Anchor names declared anywhere under src/whim/. Cover both static and dynamic
# JSX forms by collecting all data-tour-anchor occurrences and the next quoted
# string token on the same line.
declared=$(grep -rEho 'data-tour-anchor[^a-zA-Z0-9_-][^>]*' "${WHIM_DIR}" \
    --include='*.tsx' --include='*.ts' \
    --exclude="steps.ts" \
    | grep -Eo "['\"][a-z0-9-]+['\"]" \
    | tr -d "\"'" \
    | sort -u)

missing=$(comm -23 <(printf '%s\n' "${referenced}") <(printf '%s\n' "${declared}") || true)

status=0
if [[ -n "${missing}" ]]; then
    echo "lint:tour: tour steps reference anchors with no matching data-tour-anchor declaration:"
    printf '  - %s\n' ${missing}
    status=1
fi

if [[ ${status} -eq 0 ]]; then
    count=$(printf '%s\n' "${referenced}" | wc -l | tr -d ' ')
    echo "lint:tour: ok (${count} anchors referenced, all resolve)"
fi

exit ${status}
