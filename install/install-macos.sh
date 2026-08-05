#!/usr/bin/env bash
# install-macos.sh: install or uninstall the turbofig LaunchAgent on macOS.
# Usage:
#   ./install/install-macos.sh           # install
#   ./install/install-macos.sh --uninstall  # uninstall
set -euo pipefail

LABEL="eu.lukehawkins.turbofig"
PLIST_TEMPLATE="$(cd "$(dirname "$0")" && pwd)/eu.lukehawkins.turbofig.plist"
LAUNCH_AGENTS_DIR="${HOME}/Library/LaunchAgents"
PLIST_DEST="${LAUNCH_AGENTS_DIR}/${LABEL}.plist"
LOG_DIR="${HOME}/Library/Logs/turbofig"
LOG_OUT="${LOG_DIR}/turbofig.out.log"
LOG_ERR="${LOG_DIR}/turbofig.err.log"

# ---------------------------------------------------------------------------
# Uninstall path
# ---------------------------------------------------------------------------
if [[ "${1:-}" == "--uninstall" ]]; then
    echo "Uninstalling ${LABEL}..."
    launchctl unload -w "${PLIST_DEST}" 2>/dev/null || true
    if [[ -f "${PLIST_DEST}" ]]; then
        rm "${PLIST_DEST}"
        echo "Removed ${PLIST_DEST}"
    else
        echo "No plist found at ${PLIST_DEST}. Nothing to remove."
    fi
    echo "Uninstall complete."
    echo "Log files remain at ${LOG_DIR}. Remove them manually if you do not need them."
    exit 0
fi

# ---------------------------------------------------------------------------
# Resolve the daemon binary
# ---------------------------------------------------------------------------
if [[ -n "${TURBOFIG_BIN:-}" ]]; then
    DAEMON_BIN="${TURBOFIG_BIN}"
    echo "Using TURBOFIG_BIN override: ${DAEMON_BIN}"
else
    # Resolve the repo root relative to this script.
    SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
    REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
    RELEASE_BIN="${REPO_ROOT}/target/release/turbofig-mcp"

    if [[ -x "${RELEASE_BIN}" ]]; then
        DAEMON_BIN="${RELEASE_BIN}"
        echo "Found release binary: ${DAEMON_BIN}"
    else
        echo "Release binary not found. Building with cargo..."
        cargo build --release --manifest-path "${REPO_ROOT}/Cargo.toml"
        DAEMON_BIN="${RELEASE_BIN}"
        echo "Build complete: ${DAEMON_BIN}"
    fi
fi

if [[ ! -x "${DAEMON_BIN}" ]]; then
    echo "ERROR: Binary not executable or not found: ${DAEMON_BIN}" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# Prepare directories
# ---------------------------------------------------------------------------
mkdir -p "${LAUNCH_AGENTS_DIR}"
mkdir -p "${LOG_DIR}"

# ---------------------------------------------------------------------------
# Instantiate the plist from the template
# ---------------------------------------------------------------------------
if [[ ! -f "${PLIST_TEMPLATE}" ]]; then
    echo "ERROR: Plist template not found: ${PLIST_TEMPLATE}" >&2
    exit 1
fi

sed \
    -e "s|__TURBOFIG_BIN__|${DAEMON_BIN}|g" \
    -e "s|__TURBOFIG_LOG_OUT__|${LOG_OUT}|g" \
    -e "s|__TURBOFIG_LOG_ERR__|${LOG_ERR}|g" \
    "${PLIST_TEMPLATE}" > "${PLIST_DEST}"

echo "Wrote plist: ${PLIST_DEST}"

# ---------------------------------------------------------------------------
# Load the service (idempotent: unload first, ignore errors)
# ---------------------------------------------------------------------------
launchctl unload -w "${PLIST_DEST}" 2>/dev/null || true
launchctl load -w "${PLIST_DEST}"

echo ""
echo "turbofig is installed and running."
echo ""
echo "Check status:"
echo "  launchctl list | grep turbofig"
echo ""
echo "View logs:"
echo "  tail -f ${LOG_OUT}"
echo "  tail -f ${LOG_ERR}"
echo ""
echo "To uninstall:"
echo "  ${BASH_SOURCE[0]} --uninstall"
