#!/bin/sh
# Regenerates the `cli_oracle_matrix` case corpus.
#
# Each case is one directory holding `argv` (one argument per line, an empty
# file meaning no arguments) and `class`, the frozen classification the gate
# re-derives and compares against:
#
#   parity      — Go and Rust agree on stdout, stderr and exit status.
#   gap         — Rust does not implement the surface and refuses; `rust-exit`
#                 and `rust-stderr` pin the exact refusal, and the gate proves
#                 Go *does* implement it, so the row records a Rust absence
#                 rather than a shared one. A gap is a recorded row, never a
#                 skip.
#   rejected-both — both refuse (non-zero, empty stdout) with different
#                 wording; both first lines are pinned.
#   divergence  — both implement it and disagree; `go-first-line` and
#                 `rust-first-line` pin the disagreement and `reason` says why
#                 it is recorded rather than fixed here.
#
# The gate fails when an observed classification differs from the frozen one in
# either direction, so a gap that closes is as red as a parity that breaks.
#
#   ORCHESTRATOR_ACCEPTED_GO_BIN=<frozen in-tree go binary> \
#   RUST_BIN=<target/debug/orchestrator> ./regenerate.sh
set -eu

here=$(cd "$(dirname "$0")" && pwd -P)
cases="$here/cases"
go_bin=${ORCHESTRATOR_ACCEPTED_GO_BIN:?ORCHESTRATOR_ACCEPTED_GO_BIN must name the frozen in-tree Go oracle}
rust_bin=${RUST_BIN:?RUST_BIN must name the built Rust orchestrator}

emit() {
  name=$1
  shift
  directory="$cases/$name"
  mkdir -p "$directory"
  : >"$directory/argv"
  for argument in "$@"; do printf '%s\n' "$argument" >>"$directory/argv"; done

  fixture=$(cd "$(mktemp -d)" && pwd -P)
  mkdir -p "$fixture/home" "$fixture/config" "$fixture/personas" "$fixture/path"
  go_out=$(cd "$fixture" && env -i HOME="$fixture/home" \
    ORCHESTRATOR_CONFIG_DIR="$fixture/config" \
    ORCHESTRATOR_PERSONAS_DIR="$fixture/personas" \
    PATH="$fixture/path" "$go_bin" "$@" 2>"$fixture/go.err") && go_exit=0 || go_exit=$?
  rust_out=$(cd "$fixture" && env -i HOME="$fixture/home" \
    ORCHESTRATOR_CONFIG_DIR="$fixture/config" \
    ORCHESTRATOR_PERSONAS_DIR="$fixture/personas" \
    PATH="$fixture/path" "$rust_bin" "$@" 2>"$fixture/rust.err") && rust_exit=0 || rust_exit=$?
  go_err=$(cat "$fixture/go.err")
  rust_err=$(cat "$fixture/rust.err")

  rm -f "$directory/rust-exit" "$directory/rust-stderr" \
        "$directory/go-first-line" "$directory/rust-first-line"
  go_first=$(printf '%s\n%s' "$go_out" "$go_err" | sed '/^[[:space:]]*$/d' | head -1)
  rust_first=$(printf '%s\n%s' "$rust_out" "$rust_err" | sed '/^[[:space:]]*$/d' | head -1)
  if [ "$go_exit" = "$rust_exit" ] && [ "$go_out" = "$rust_out" ] && [ "$go_err" = "$rust_err" ]; then
    printf 'parity\n' >"$directory/class"
  elif [ "$go_exit" = 0 ] && [ -z "$rust_out" ] && [ "$rust_exit" != 0 ]; then
    printf 'gap\n' >"$directory/class"
    printf '%s\n' "$rust_exit" >"$directory/rust-exit"
    printf '%s\n' "$rust_first" >"$directory/rust-stderr"
  elif [ "$go_exit" != 0 ] && [ "$rust_exit" != 0 ] && [ -z "$go_out" ] && [ -z "$rust_out" ]; then
    printf 'rejected-both\n' >"$directory/class"
    printf '%s\n' "$go_first" >"$directory/go-first-line"
    printf '%s\n' "$rust_first" >"$directory/rust-first-line"
  else
    printf 'divergence\n' >"$directory/class"
    printf '%s\n' "$go_first" >"$directory/go-first-line"
    printf '%s\n' "$rust_first" >"$directory/rust-first-line"
    [ -f "$directory/reason" ] || printf 'undocumented divergence\n' >"$directory/reason"
  fi
  # A `reason` belongs only to a divergence row. A row that stops diverging must
  # not keep a stale one, or the corpus documents a claim it no longer makes;
  # `every_row_that_carries_a_reason_is_a_divergence` enforces that from the
  # gate side.
  [ "$(cat "$directory/class")" = divergence ] || rm -f "$directory/reason"
  rm -rf "$fixture"
}

mkdir -p "$cases"

emit root-no-arguments
emit root-help --help
emit root-help-short -h
emit root-double-dash --
emit root-double-dash-run -- run --help
emit root-help-word help
emit root-version --version
emit root-version-short -V
emit root-unknown-command frobnicate
emit root-unknown-flag --wat
emit root-unknown-flag-with-value --wat=x run --help
emit root-unknown-shorthand -Z

for command in advisor archive audit backfill-embeddings barok cancel cleanup \
               compare completion daemon discipline doctor dream events evidence \
               hooks ingest memory metrics prune routing run stats status sync \
               templates; do
  emit "command-$command-help" "$command" --help
  emit "command-$command-unregistered-flag" "$command" --definitely-not-a-flag
done

emit flag-domain-before-run --domain work run --help
emit flag-domain-inline-after-run run --domain=work --help
emit flag-nanika-dir-before-run --nanika-dir /fixture/nanika run --help
emit flag-nanika-dir-inline-after-run run --nanika-dir=/fixture/nanika --help
emit flag-bundle-before-run --dry-run=true --max-turns=3 -v=false run --help
emit flag-bundle-after-run run --dry-run=false --max-turns=3 -v=true --help
emit flag-shorthand-cluster -vh run
emit flag-shorthand-cluster-false -vh=false run --help
emit flag-help-word-with-domain --domain work help run
emit flag-help-word-domain-between help --domain work run
emit flag-help-word-domain-after help run --domain work
emit flag-runtime-before-run --runtime codex run --help
emit flag-negative-max-turns --max-turns=-1 run --help
emit flag-invalid-max-turns --max-turns nope run --help
emit flag-invalid-bool --dry-run=maybe
emit flag-missing-value --domain
emit flag-run-missing-runtime-value run --runtime
emit run-no-task run
emit run-unknown-flag run --wat
emit run-help-then-unknown run --help --wat
emit run-unknown-then-help run --wat --help

printf 'regenerated %s cases\n' "$(find "$cases" -name class | wc -l | tr -d ' ')"
