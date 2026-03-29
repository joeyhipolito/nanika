#!/usr/bin/env bash
# install.sh — build nen scanner binaries and install to <config-dir>/nen/scanners/
set -euo pipefail

PLUGIN_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

resolve_config_dir() {
    if [[ -n "${ORCHESTRATOR_CONFIG_DIR:-}" ]]; then
        printf '%s\n' "${ORCHESTRATOR_CONFIG_DIR}"
        return
    fi
    if [[ -n "${ALLUKA_HOME:-}" ]]; then
        printf '%s\n' "${ALLUKA_HOME}"
        return
    fi
    if [[ -n "${VIA_HOME:-}" ]]; then
        printf '%s\n' "${VIA_HOME}/orchestrator"
        return
    fi
    if [[ -d "${HOME}/.alluka" ]]; then
        printf '%s\n' "${HOME}/.alluka"
        return
    fi
    printf '%s\n' "${HOME}/.via"
}

CONFIG_DIR="$(resolve_config_dir)"
SCANNERS_DIR="${CONFIG_DIR}/nen/scanners"

echo "Building nen scanners..."
cd "$PLUGIN_DIR"

mkdir -p "$SCANNERS_DIR"

for scanner in gyo en ryu; do
    echo "  building $scanner..."
    GOWORK=off go build -o "${SCANNERS_DIR}/${scanner}" "./cmd/${scanner}"
    chmod +x "${SCANNERS_DIR}/${scanner}"
    echo "  installed: ${SCANNERS_DIR}/${scanner}"
done

cp "${PLUGIN_DIR}/plugin.json" "${CONFIG_DIR}/nen/plugin.json"
echo "  installed: ${CONFIG_DIR}/nen/plugin.json"

echo ""
echo "Building shu evaluator..."
GOWORK=off go build -o "${CONFIG_DIR}/bin/shu" "./cmd/shu"
chmod +x "${CONFIG_DIR}/bin/shu"
echo "  installed: ${CONFIG_DIR}/bin/shu"

echo ""
echo "Building ko evaluator..."
GOWORK=off go build -o "${CONFIG_DIR}/bin/ko" "./cmd/ko"
chmod +x "${CONFIG_DIR}/bin/ko"
echo "  installed: ${CONFIG_DIR}/bin/ko"

echo ""
echo "Building nen-daemon..."
GOWORK=off go build -o "${CONFIG_DIR}/bin/nen-daemon" "./cmd/nen-daemon"
chmod +x "${CONFIG_DIR}/bin/nen-daemon"
echo "  installed: ${CONFIG_DIR}/bin/nen-daemon"

echo ""
echo "Done. Scanners installed to ${SCANNERS_DIR}/"
echo "shu installed to ${CONFIG_DIR}/bin/shu"
echo "nen-daemon installed to ${CONFIG_DIR}/bin/nen-daemon"
echo ""
echo "Run 'nen-daemon start' to subscribe to orchestrator events."
echo "Run 'shu evaluate' to check nanika component health."
