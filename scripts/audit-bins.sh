#!/usr/bin/env bash
# scripts/audit-bins.sh — verify every plugin + skill binary installs to ~/.alluka/bin.
#
# Scans plugin.json install fields, the top-level Makefile, skill Makefiles,
# and scripts/install.sh for any ln/cp/go-install that targets a location
# outside ~/.alluka/ (e.g. ~/bin, $GOPATH/bin, ~/nanika/bin). Exits non-zero
# when a violation is found so CI or a local pre-commit hook can catch drift.
#
# Why this exists: install destinations have drifted multiple times (~/bin,
# $(HOME)/bin, GOBIN-default, ~/nanika/bin) — workers expect binaries at
# ~/.alluka/bin and silently pick up stale copies when install paths diverge.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
violations=0

fail() {
  printf 'violation: %s\n' "$*" >&2
  violations=$((violations + 1))
}

# 1. Every plugin.json install field targets ~/.alluka/bin (or nen's install.sh
#    which we inspect separately). Exception: plugins without an install field
#    (e.g. dust, GUI plugins) are allowed.
for f in "$REPO_ROOT"/plugins/*/plugin.json; do
  plugin=$(basename "$(dirname "$f")")
  install=$(jq -r '.install // ""' "$f" 2>/dev/null)
  [[ -z "$install" ]] && continue
  # Allow nen's install.sh — checked separately below.
  [[ "$install" == "bash install.sh" ]] && continue
  if [[ "$install" != *".alluka/bin"* ]]; then
    fail "$plugin plugin.json install does not target ~/.alluka/bin: $install"
  fi
done

# 2. Top-level Makefile and skill Makefiles must not symlink into ~/bin or
#    fall back to GOPATH/bin for install targets.
for mf in "$REPO_ROOT/Makefile" "$REPO_ROOT"/skills/*/Makefile; do
  [[ -f "$mf" ]] || continue
  # Flag ln/mkdir/ln-sf into $(HOME)/bin but allow $(HOME)/.alluka/bin.
  if grep -E 'ln -sf.*\$\(HOME\)/bin/|mkdir.*\$\(HOME\)/bin(\s|$)' "$mf" >/dev/null 2>&1; then
    fail "$mf targets \$(HOME)/bin — should be \$(HOME)/.alluka/bin"
  fi
  # Flag bare `go install` (no GOBIN) — would land in $GOPATH/bin.
  if grep -E '^\s*(cd .+ && )?go install( |$)' "$mf" >/dev/null 2>&1 \
      && ! grep -E 'GOBIN=.+/.alluka/bin' "$mf" >/dev/null 2>&1; then
    fail "$mf uses bare 'go install' without GOBIN=~/.alluka/bin"
  fi
done

# 3. nen's install.sh must target ~/.alluka/ for bin and scanners.
nen_sh="$REPO_ROOT/plugins/nen/install.sh"
if [[ -f "$nen_sh" ]]; then
  if grep -E 'ln -sf|go build -o' "$nen_sh" | grep -E '\$HOME/bin|/usr/local/bin' >/dev/null 2>&1; then
    fail "$nen_sh writes outside ~/.alluka/"
  fi
fi

# 4. scripts/install.sh must not create ~/bin mirrors of ~/.alluka/bin entries.
user_sh="$REPO_ROOT/scripts/install.sh"
if [[ -f "$user_sh" ]]; then
  if grep -E 'ln -sf "\$HOME/\.alluka/bin/\$?[a-zA-Z_{}]*" "\$HOME/bin' "$user_sh" >/dev/null 2>&1; then
    fail "$user_sh creates ~/bin mirror symlinks of ~/.alluka/bin entries"
  fi
fi

if [[ $violations -eq 0 ]]; then
  printf 'audit-bins: ok (all install destinations under ~/.alluka/)\n'
  exit 0
fi

printf '\naudit-bins: %d violation(s) found\n' "$violations" >&2
exit 1
