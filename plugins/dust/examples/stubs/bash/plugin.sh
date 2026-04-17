#!/usr/bin/env bash
# plugin.sh — minimal dust v1 stub (socat + jq + framing loop)
#
# Minimum surface: ready event, host_info handshake, heartbeat echo,
# manifest/render/action responses, shutdown.
# Requires: bash ≥ 4, socat, jq, xxd, date
#
# Usage:
#   ./plugin.sh [--socket PATH]   # listen on UDS socket
#   ./plugin.sh --handle          # internal: handle one connection on stdin/stdout
set -euo pipefail

PLUGIN_ID="bash-stub"
SEQ=0

# Extend PATH so socat/jq/xxd are found even in a minimal environment.
export PATH="/opt/homebrew/bin:/usr/local/bin:${PATH}"

die()     { echo "dust: $*" >&2; exit 1; }
now_ts()  { date -u '+%Y-%m-%dT%H:%M:%S.000Z'; }
next_seq(){ SEQ=$((SEQ + 1)); echo "$SEQ"; }
seq_id()  { printf '%016x' "$1"; }

runtime_dir() {
    [ -n "${XDG_RUNTIME_DIR:-}" ] \
        && printf '%s/nanika/plugins' "${XDG_RUNTIME_DIR}" \
        || printf '%s/.alluka/run/plugins' "${HOME}"
}

# write_u32 N — write 4-byte big-endian uint32 to stdout.
# Uses octal escapes: printf '\NNN' avoids the invalid \x%02x pattern.
write_u32() {
    local n=$1
    printf "\\$(printf '%03o' $(( (n >> 24) & 0xFF )))\\$(printf '%03o' $(( (n >> 16) & 0xFF )))\\$(printf '%03o' $(( (n >> 8) & 0xFF )))\\$(printf '%03o' $(( n & 0xFF )))"
}

# write_frame JSON — length-prefixed JSON frame to stdout.
write_frame() {
    write_u32 "${#1}"
    printf '%s' "$1"
}

# read_u32 — read 4 bytes big-endian uint32 from stdin, print decimal; 1 on EOF.
read_u32() {
    local h
    h=$(dd bs=1 count=4 2>/dev/null | xxd -p | tr -d ' \n')
    [ ${#h} -lt 8 ] && return 1
    echo $(( (16#${h:0:2} << 24) | (16#${h:2:2} << 16) | (16#${h:4:2} << 8) | 16#${h:6:2} ))
}

# read_frame — one length-prefixed frame from stdin → JSON; 1 on EOF.
read_frame() {
    local n
    n=$(read_u32) || return 1
    [ "$n" -le 0 ] && return 1
    dd bs=1 count="$n" 2>/dev/null
}

MANIFEST_JSON='{"name":"Bash Stub","version":"0.1.0","description":"Minimal bash stub implementing the dust v1 handshake.","capabilities":[{"kind":"command","prefix":"bash-stub"}],"icon":null}'

make_ready() {
    local seq ts
    seq=$(next_seq); ts=$(now_ts)
    jq -cn --arg id "evt_$(seq_id "$seq")" --arg ts "$ts" \
        --argjson seq "$seq" --argjson pid "$$" --argjson mf "$MANIFEST_JSON" \
        '{kind:"event",id:$id,type:"ready",ts:$ts,sequence:$seq,
          data:{manifest:$mf,protocol_version:"1.0.0",
                plugin_info:{pid:$pid,started_at:$ts}}}'
}

dispatch() {
    local id method
    id=$(printf '%s' "$1" | jq -r '.id')
    method=$(printf '%s' "$1" | jq -r '.method')
    case "$method" in
        manifest|refresh_manifest)
            jq -cn --arg id "$id" --argjson mf "$MANIFEST_JSON" \
                '{kind:"response",id:$id,result:$mf}' ;;
        render)
            jq -cn --arg id "$id" '{kind:"response",id:$id,result:[]}' ;;
        action)
            jq -cn --arg id "$id" '{kind:"response",id:$id,result:{success:true}}' ;;
        *)
            jq -cn --arg id "$id" --arg m "$method" \
                '{kind:"response",id:$id,error:{code:-32601,message:("method not found: "+$m)}}' ;;
    esac
}

handle_connection() {
    write_frame "$(make_ready)"

    # Read host_info with 5-second timeout.
    # NOTE: use `bash -c '...'` (not `bash -s <<'EOF'`) so stdin stays connected
    # to the socket — heredoc would redirect stdin to the script text instead.
    local hi_json
    hi_json=$(timeout 5 bash -c '
        export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
        h=$(dd bs=1 count=4 2>/dev/null | xxd -p | tr -d " \n")
        [ ${#h} -lt 8 ] && exit 1
        n=$(( (16#${h:0:2} << 24) | (16#${h:2:2} << 16) | (16#${h:4:2} << 8) | 16#${h:6:2} ))
        [ "$n" -le 0 ] && exit 1
        dd bs=1 count="$n" 2>/dev/null
    ' 2>/dev/null) || { echo "dust: host_info timeout or read error" >&2; return; }

    local hk ht
    hk=$(printf '%s' "$hi_json" | jq -r '.kind // ""')
    ht=$(printf '%s' "$hi_json" | jq -r '.type // ""')
    [ "$hk" = "event" ] && [ "$ht" = "host_info" ] \
        || { echo "dust: expected host_info, got kind=$hk type=$ht" >&2; return; }

    local frame kind
    while frame=$(read_frame 2>/dev/null); do
        [ -z "$frame" ] && break
        kind=$(printf '%s' "$frame" | jq -r '.kind // ""')
        case "$kind" in
            request)   write_frame "$(dispatch "$frame")" ;;
            heartbeat) write_frame "$(jq -cn --arg ts "$(now_ts)" '{kind:"heartbeat",ts:$ts}')" ;;
            shutdown)  return ;;
        esac
    done
}

main() {
    local sock_path=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --handle)  handle_connection; return ;;
            --socket)  shift; sock_path="$1" ;;
            *)         die "unknown argument: $1" ;;
        esac
        shift
    done

    [ -z "$sock_path" ] && sock_path="$(runtime_dir)/${PLUGIN_ID}.sock"

    command -v socat >/dev/null 2>&1 || die "socat not found in PATH"
    command -v jq    >/dev/null 2>&1 || die "jq not found in PATH"
    command -v xxd   >/dev/null 2>&1 || die "xxd not found in PATH"

    mkdir -p "$(dirname "$sock_path")"
    rm -f "$sock_path"
    trap 'rm -f "$sock_path"' EXIT INT TERM

    echo "dust: ${PLUGIN_ID} listening on ${sock_path}" >&2
    exec socat "UNIX-LISTEN:${sock_path},fork" "EXEC:bash $0 --handle,nofork"
}

main "$@"
