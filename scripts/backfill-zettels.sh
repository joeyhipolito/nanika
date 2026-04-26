#!/usr/bin/env bash
# backfill-zettels.sh — replay historical mission.completed events through
# `obsidian zettel write mission` so missions that completed before the
# ZettelHook was wired get vault entries retroactively.
#
# Usage:
#   scripts/backfill-zettels.sh [--dry-run]
#
# Behavior (see ~/.alluka/missions/2026-04-22-backfill-missed-zettels.md):
#   - Walks every ~/.alluka/events/*.jsonl
#   - For each file, finds the mission.completed event and builds a
#     MissionPayload matching skills/orchestrator/internal/engine/zettel_hooks.go
#   - Skips when ~/.alluka/vault/missions/<YYYY-MM-DD>-<slug>.md already exists
#   - Invokes `obsidian zettel write mission --dropped-dir /tmp/backfill-dropped`
#     with payload on stdin and OBSIDIAN_VAULT_PATH=$HOME/.alluka/vault
#   - Logs one line per mission to shared/artifacts/backfill-zettels.log:
#       wrote <id> <vault-path>
#       skipped <id> <reason>
#       failed <id> <reason>
#   - --dry-run prints the plan (including the resolved MissionPayload) without
#     invoking the CLI.

set -euo pipefail

DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    -h|--help)
      sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

HOME_DIR="${HOME}"
EVENTS_DIR="${HOME_DIR}/.alluka/events"
VAULT_DIR="${HOME_DIR}/.alluka/vault"
VAULT_MISSIONS_DIR="${VAULT_DIR}/missions"
WORKSPACES_DIR="${HOME_DIR}/.alluka/workspaces"
MISSIONS_DIR="${HOME_DIR}/.alluka/missions"
DROPPED_DIR="/tmp/backfill-dropped"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
LOG_DIR="${REPO_ROOT}/shared/artifacts"
LOG_FILE="${LOG_DIR}/backfill-zettels.log"

mkdir -p "${LOG_DIR}"
if [[ "${DRY_RUN}" -eq 0 ]]; then
  mkdir -p "${DROPPED_DIR}"
  : >"${LOG_FILE}"
fi

for dep in jq; do
  if ! command -v "$dep" >/dev/null 2>&1; then
    echo "missing dependency: $dep" >&2
    exit 1
  fi
done

OBSIDIAN_BIN="${OBSIDIAN_BIN:-$(command -v obsidian || true)}"
if [[ "${DRY_RUN}" -eq 0 && -z "${OBSIDIAN_BIN}" ]]; then
  echo "obsidian CLI not found on PATH; set OBSIDIAN_BIN" >&2
  exit 1
fi

# log_line <verb> <id> <message> — prints to stdout and (unless dry-run) the log file.
log_line() {
  local verb="$1" id="$2" msg="$3"
  local line="${verb} ${id} ${msg}"
  printf '%s\n' "$line"
  if [[ "${DRY_RUN}" -eq 0 ]]; then
    printf '%s\n' "$line" >>"${LOG_FILE}"
  fi
}

# derive_slug <mission_id> — resolves the slug by inspecting, in order:
#   1. workspaces/<id>/mission_path basename (strips leading YYYY-MM-DD-)
#   2. missions/<YYYY-MM-DD>-*.md for that date (if single match)
# Prints the resolved slug, or empty string if none found.
derive_slug() {
  local id="$1"
  local mission_path="${WORKSPACES_DIR}/${id}/mission_path"
  if [[ -f "${mission_path}" ]]; then
    local p
    p="$(tr -d '\n' <"${mission_path}")"
    if [[ -n "$p" ]]; then
      local base
      base="$(basename "$p" .md)"
      # strip leading YYYY-MM-DD-
      printf '%s' "${base#????-??-??-}"
      return
    fi
  fi
  printf ''
}

# derive_personas <mission_id> — reads plan.json and emits a JSON array of
# unique persona names. Falls back to [] if plan.json is missing or unreadable.
derive_personas() {
  local id="$1"
  local plan="${WORKSPACES_DIR}/${id}/plan.json"
  if [[ -f "${plan}" ]]; then
    jq -c '[.phases[]?.persona // empty] | unique' "${plan}" 2>/dev/null || printf '[]'
  else
    printf '[]'
  fi
}

# derive_trackers <mission_id> — greps TRK-<digits> from the task text in
# plan.json / mission.md and emits a unique JSON array.
derive_trackers() {
  local id="$1"
  local plan="${WORKSPACES_DIR}/${id}/plan.json"
  local mission_md="${WORKSPACES_DIR}/${id}/mission.md"
  local src=""
  if [[ -f "${plan}" ]]; then
    src="$(jq -r '.task // ""' "${plan}" 2>/dev/null || true)"
  fi
  if [[ -z "${src}" && -f "${mission_md}" ]]; then
    src="$(cat "${mission_md}")"
  fi
  if [[ -z "${src}" ]]; then
    printf '[]'
    return
  fi
  # grep returns 1 when there are no matches — tolerate that and emit [].
  local matches
  matches="$(printf '%s' "${src}" | grep -oE 'TRK-[0-9]+' | sort -u || true)"
  if [[ -z "${matches}" ]]; then
    printf '[]'
    return
  fi
  printf '%s\n' "${matches}" | jq -R . | jq -sc .
}

# derive_artifacts_from_event <jsonl_file> — emits the data.artifacts_list
# array from the mission.completed event, or [] if absent.
derive_artifacts_from_event() {
  local jsonl="$1"
  local out
  out="$(jq -c 'select(.type=="mission.completed") | .data.artifacts_list // []' "${jsonl}" 2>/dev/null || true)"
  # take the first non-empty line
  out="$(printf '%s\n' "${out}" | awk 'NF {print; exit}')"
  if [[ -z "${out}" ]]; then
    printf '[]'
  else
    printf '%s' "${out}"
  fi
}

# derive_artifacts_from_disk <mission_id> — fallback: lists files under
# workspaces/<id>/artifacts/merged as absolute paths.
derive_artifacts_from_disk() {
  local id="$1"
  local merged="${WORKSPACES_DIR}/${id}/artifacts/merged"
  if [[ -d "${merged}" ]]; then
    local files
    files="$(find "${merged}" -type f -print 2>/dev/null | sort || true)"
    if [[ -z "${files}" ]]; then
      printf '[]'
    else
      printf '%s\n' "${files}" | jq -R . | jq -sc .
    fi
  else
    printf '[]'
  fi
}

shopt -s nullglob
jsonl_files=( "${EVENTS_DIR}"/[0-9]*.jsonl )
shopt -u nullglob

if [[ ${#jsonl_files[@]} -eq 0 ]]; then
  echo "no mission event files under ${EVENTS_DIR}" >&2
  exit 0
fi

for jsonl in "${jsonl_files[@]}"; do
  fname="$(basename "${jsonl}" .jsonl)"
  mission_id="${fname}"

  # 1. find mission.completed event (first one — there should only be one)
  completed_event="$(jq -c 'select(.type=="mission.completed")' "${jsonl}" 2>/dev/null | head -n 1 || true)"
  if [[ -z "${completed_event}" ]]; then
    log_line skipped "${mission_id}" "no mission.completed event"
    continue
  fi

  completed_ts="$(printf '%s' "${completed_event}" | jq -r '.timestamp')"
  if [[ -z "${completed_ts}" || "${completed_ts}" == "null" ]]; then
    log_line skipped "${mission_id}" "mission.completed has no timestamp"
    continue
  fi
  completed_date="${completed_ts:0:10}"

  # 2. resolve slug (fallback to mission-<id>)
  slug="$(derive_slug "${mission_id}")"
  slug_was_fallback=0
  if [[ -z "${slug}" ]]; then
    slug="mission-${mission_id}"
    slug_was_fallback=1
  fi

  vault_path="${VAULT_MISSIONS_DIR}/${completed_date}-${slug}.md"

  # 3. idempotent skip
  if [[ -e "${vault_path}" ]]; then
    log_line skipped "${mission_id}" "already exists: ${vault_path}"
    continue
  fi

  # 4. derive other payload fields
  personas_json="$(derive_personas "${mission_id}")"
  trackers_json="$(derive_trackers "${mission_id}")"
  artifacts_json="$(derive_artifacts_from_event "${jsonl}")"
  if [[ -z "${artifacts_json}" || "${artifacts_json}" == "[]" ]]; then
    artifacts_json="$(derive_artifacts_from_disk "${mission_id}")"
  fi

  # 5. build MissionPayload JSON
  payload="$(jq -cn \
    --arg id "${mission_id}" \
    --arg slug "${slug}" \
    --arg completed "${completed_ts}" \
    --argjson personas "${personas_json:-[]}" \
    --argjson trackers "${trackers_json:-[]}" \
    --argjson artifacts "${artifacts_json:-[]}" \
    '{id:$id, slug:$slug, completed:$completed, personas:$personas, trackers:$trackers, artifacts:$artifacts}')"

  if [[ "${DRY_RUN}" -eq 1 ]]; then
    verdict="wrote"
    msg="would write ${vault_path}"
    if [[ "${slug_was_fallback}" -eq 1 ]]; then
      msg="${msg} (slug=fallback)"
    fi
    log_line "${verdict}" "${mission_id}" "${msg}"
    # emit the resolved payload on the next line for reviewer inspection
    printf '  payload: %s\n' "${payload}"
    continue
  fi

  # 6. invoke CLI
  if ! cli_out="$(printf '%s' "${payload}" | OBSIDIAN_VAULT_PATH="${VAULT_DIR}" \
      "${OBSIDIAN_BIN}" zettel write mission --dropped-dir "${DROPPED_DIR}" 2>&1)"; then
    log_line failed "${mission_id}" "cli error: $(printf '%s' "${cli_out}" | tr '\n' ' ')"
    continue
  fi

  # 7. parse CLI response
  cli_error="$(printf '%s' "${cli_out}" | jq -r '.error // ""' 2>/dev/null || printf '')"
  cli_path="$(printf '%s' "${cli_out}" | jq -r '.path // ""' 2>/dev/null || printf '')"
  cli_skipped="$(printf '%s' "${cli_out}" | jq -r '.skipped // false' 2>/dev/null || printf 'false')"
  cli_skip_reason="$(printf '%s' "${cli_out}" | jq -r '.skip_reason // ""' 2>/dev/null || printf '')"
  cli_dropped="$(printf '%s' "${cli_out}" | jq -r '.dropped // false' 2>/dev/null || printf 'false')"
  cli_dropped_path="$(printf '%s' "${cli_out}" | jq -r '.dropped_path // ""' 2>/dev/null || printf '')"

  if [[ -n "${cli_error}" && "${cli_error}" != "null" ]]; then
    log_line failed "${mission_id}" "${cli_error}"
    continue
  fi
  if [[ "${cli_dropped}" == "true" ]]; then
    log_line failed "${mission_id}" "dropped to ${cli_dropped_path}"
    continue
  fi
  if [[ "${cli_skipped}" == "true" ]]; then
    log_line skipped "${mission_id}" "${cli_skip_reason:-cli-skipped}"
    continue
  fi

  log_line wrote "${mission_id}" "${cli_path}"
done

if [[ "${DRY_RUN}" -eq 0 ]]; then
  printf 'done. log: %s\n' "${LOG_FILE}"
fi
