#!/usr/bin/env bash
# lint-scenes-registration.sh — Verify every Scene exported from
# src/whim/scenarios/Scenes.tsx (including `export { X as YyyScene }` re-exports)
# is registered in the SCENE_COMPONENTS map in src/whim/App.tsx, and that every
# map entry resolves to an actual export.
#
# Exit codes:
#   0  registration is consistent
#   1  drift detected (unregistered exports OR map entries without an export)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLUGIN_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

SCENES_FILE="${PLUGIN_ROOT}/src/whim/scenarios/Scenes.tsx"
APP_FILE="${PLUGIN_ROOT}/src/whim/App.tsx"

if [[ ! -f "${SCENES_FILE}" ]]; then
    echo "lint:scenes: missing ${SCENES_FILE}" >&2
    exit 1
fi
if [[ ! -f "${APP_FILE}" ]]; then
    echo "lint:scenes: missing ${APP_FILE}" >&2
    exit 1
fi

# Collect Scene exports from Scenes.tsx:
#   1. `export function FooScene(...)`
#   2. `export const FooScene = ...`
#   3. `export { Foo as FooScene } from '...'`  /  `export { FooScene } from '...'`
exported=$( {
    grep -Eo '^export (function|const) [A-Za-z0-9_]+Scene' "${SCENES_FILE}" \
        | awk '{print $3}'
    # Re-export forms — capture every identifier ending in "Scene" that appears
    # inside an `export { ... }` block (covers `as XxxScene` and bare `XxxScene`).
    grep -Eo '^export \{[^}]*\}' "${SCENES_FILE}" \
        | grep -Eo '[A-Za-z0-9_]+Scene'
} | sort -u)

# Components referenced from the SCENE_COMPONENTS map.
registered=$(awk '/const SCENE_COMPONENTS/,/^}/' "${APP_FILE}" \
    | grep -Eo '[A-Za-z0-9_]+Scene' \
    | sort -u)

missing_in_map=$(comm -23 <(printf '%s\n' "${exported}") <(printf '%s\n' "${registered}") || true)
missing_in_exports=$(comm -13 <(printf '%s\n' "${exported}") <(printf '%s\n' "${registered}") || true)

status=0
if [[ -n "${missing_in_map}" ]]; then
    echo "lint:scenes: scenes exported from Scenes.tsx but not registered in SCENE_COMPONENTS:"
    printf '  - %s\n' ${missing_in_map}
    status=1
fi
if [[ -n "${missing_in_exports}" ]]; then
    echo "lint:scenes: SCENE_COMPONENTS references symbols with no matching export in Scenes.tsx:"
    printf '  - %s\n' ${missing_in_exports}
    status=1
fi

if [[ ${status} -eq 0 ]]; then
    count=$(printf '%s\n' "${exported}" | wc -l | tr -d ' ')
    echo "lint:scenes: ok (${count} scenes registered)"
fi

exit ${status}
