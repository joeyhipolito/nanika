#!/usr/bin/env bash
# Replaces em dashes (U+2014) with ", ", cleans up double spaces and ", ." artifacts.
# Usage: strip-em-dashes.sh [--self-test] [-i|--in-place] [file]
#        strip-em-dashes.sh < input > output
set -euo pipefail

EM_DASH=$'\xe2\x80\x94'   # UTF-8 encoding of U+2014

usage() {
    printf 'Usage: %s [--self-test] [-i|--in-place] [file]\n' "$(basename "$0")" >&2
    exit 2
}

# ---------------------------------------------------------------------------
# Self-test mode
# ---------------------------------------------------------------------------
run_self_test() {
    local pass=0
    local fail=0

    check() {
        local desc="$1"
        local input="$2"
        local expected="$3"
        local got
        got=$(printf '%s' "$input" | do_transform)
        if [ "$got" = "$expected" ]; then
            printf 'PASS: %s\n' "$desc"
            pass=$((pass + 1))
        else
            printf 'FAIL: %s\n  input:    %s\n  expected: %s\n  got:      %s\n' \
                "$desc" "$input" "$expected" "$got"
            fail=$((fail + 1))
        fi
    }

    # 1. Em dash with spaces around it
    check "em dash with spaces" \
        "hello ${EM_DASH} world" \
        "hello, world"

    # 2. Em dash without spaces
    check "em dash without spaces" \
        "hello${EM_DASH}world" \
        "hello, world"

    # 3. Multiple em dashes in one line
    check "multiple em dashes" \
        "one${EM_DASH}two ${EM_DASH} three" \
        "one, two, three"

    # 4. En dash (U+2013) must be left alone
    EN_DASH=$'\xe2\x80\x93'
    check "en dash left alone" \
        "pages 10${EN_DASH}20" \
        "pages 10${EN_DASH}20"

    # 5. Hyphen must be left alone
    check "hyphen left alone" \
        "well-known fact" \
        "well-known fact"

    if [ "$fail" -gt 0 ]; then
        printf '\n%d passed, %d FAILED\n' "$pass" "$fail"
        exit 1
    fi
    printf '\n%d/%d tests passed\n' "$pass" "$((pass + fail))"
    exit 0
}

# ---------------------------------------------------------------------------
# Core transformation (stdin → stdout)
# ---------------------------------------------------------------------------
do_transform() {
    # Step 1: normalise " — ", "— ", " —", "—" all to ", "
    # Step 2: collapse double-space after comma: ",  " → ", "
    # Step 3: clean sentence-end artifact ", ." → "."
    # All done with portable sed (BRE) — safe on macOS bash 3.2 / BSD sed.
    sed \
        -e "s/ ${EM_DASH} /, /g" \
        -e "s/ ${EM_DASH}/, /g" \
        -e "s/${EM_DASH} /, /g" \
        -e "s/${EM_DASH}/, /g" \
        -e "s/,  /, /g" \
        -e "s/, \././g"
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
IN_PLACE=0
FILE=""

while [ $# -gt 0 ]; do
    case "$1" in
        --self-test)
            run_self_test
            ;;
        -i|--in-place)
            IN_PLACE=1
            shift
            ;;
        --)
            shift
            break
            ;;
        -*)
            usage
            ;;
        *)
            if [ -n "$FILE" ]; then
                usage
            fi
            FILE="$1"
            shift
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------
if [ -n "$FILE" ]; then
    if [ ! -r "$FILE" ]; then
        printf 'strip-em-dashes: cannot read file: %s\n' "$FILE" >&2
        exit 1
    fi
    if [ "$IN_PLACE" -eq 1 ]; then
        TMP=$(mktemp)
        trap 'rm -f "$TMP"' EXIT
        do_transform < "$FILE" > "$TMP"
        mv "$TMP" "$FILE"
    else
        do_transform < "$FILE"
    fi
else
    if [ "$IN_PLACE" -eq 1 ]; then
        printf 'strip-em-dashes: --in-place requires a file argument\n' >&2
        exit 2
    fi
    do_transform
fi
