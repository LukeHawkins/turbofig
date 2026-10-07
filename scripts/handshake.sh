#!/usr/bin/env bash
#
# Reproduce the exact curl handshake that the Figma curl skills use.
# The script starts the daemon, runs the MCP streamable-http handshake with
# plain curl, and checks that turbofig_status returns {"ok":true}.
#
# It proves the wire contract with the same tool a skill uses: curl. The Rust
# integration test proves the same contract in-process. This script proves it
# against the real built binary over a real socket.
#
# Usage: scripts/handshake.sh
# Exit code 0 on PASS, 1 on FAIL.

set -euo pipefail

# Use dedicated test ports and a throwaway bridge dir so the script never
# clashes with a real daemon or touches the real token. Override the ports with
# TURBOFIG_MCP_PORT / TURBOFIG_WS_PORT if needed.
PORT="${TURBOFIG_MCP_PORT:-18860}"
WS_PORT="${TURBOFIG_WS_PORT:-18861}"
BRIDGE_DIR="$(mktemp -d)"
BASE="http://127.0.0.1:${PORT}/mcp"
ACCEPT="application/json, text/event-stream"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${ROOT}/target/debug/turbofig"

fail() {
  echo "FAIL: $1" >&2
  exit 1
}

# Create the headers temp file early so cleanup can always remove it.
HEADERS="$(mktemp)"

cleanup() {
  kill "${DAEMON_PID:-}" 2>/dev/null || true
  wait "${DAEMON_PID:-}" 2>/dev/null || true
  [ -n "${HEADERS:-}" ] && rm -f "${HEADERS}"
  [ -n "${BRIDGE_DIR:-}" ] && rm -rf "${BRIDGE_DIR}"
}
trap cleanup EXIT

# Build the daemon if the binary is missing.
if [ ! -x "${BIN}" ]; then
  echo "Building the daemon..."
  (cd "${ROOT}" && cargo build --quiet)
fi

# Start the daemon in the foreground mode, backgrounded by the shell, so the
# script owns its PID. Bare `turbofig` is the first-run helper, not the daemon.
TURBOFIG_MCP_PORT="${PORT}" TURBOFIG_WS_PORT="${WS_PORT}" \
  TURBOFIG_BRIDGE_DIR="${BRIDGE_DIR}" "${BIN}" serve &
DAEMON_PID=$!

# Wait for the daemon to accept connections and write its token (up to 5 s).
READY=0
for _ in $(seq 1 50); do
  if [ -s "${BRIDGE_DIR}/token" ] \
    && curl -s -o /dev/null "http://127.0.0.1:${PORT}/mcp" 2>/dev/null; then
    READY=1
    break
  fi
  sleep 0.1
done

if [ "${READY}" -eq 0 ]; then
  fail "daemon did not become ready within 5 seconds"
fi

# /mcp requires the pairing token as a bearer token.
AUTH="Authorization: Bearer $(tr -d '\r\n' < "${BRIDGE_DIR}/token")"

# Step 1: initialize. Capture the response headers to read mcp-session-id.
INIT_BODY='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","clientInfo":{"name":"curl-skill","version":"0.1.0"},"capabilities":{}}}'
curl -s -D "${HEADERS}" -o /dev/null \
  -H "${AUTH}" \
  -H "Accept: ${ACCEPT}" \
  -H "Content-Type: application/json" \
  -d "${INIT_BODY}" \
  "${BASE}"

# Read the session id from the response headers (case-insensitive match).
SESSION_ID="$(grep -i '^mcp-session-id:' "${HEADERS}" | head -1 | sed 's/^[^:]*:[[:space:]]*//' | tr -d '\r\n' || true)"
[ -n "${SESSION_ID}" ] || fail "initialize did not return an mcp-session-id header. Headers:
$(cat "${HEADERS}")"
echo "Got session id: ${SESSION_ID}"

# Step 2: notifications/initialized with the session id.
NOTIF_BODY='{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}'
curl -s -o /dev/null \
  -H "${AUTH}" \
  -H "Accept: ${ACCEPT}" \
  -H "Content-Type: application/json" \
  -H "mcp-session-id: ${SESSION_ID}" \
  -d "${NOTIF_BODY}" \
  "${BASE}"

# Step 3: tools/call turbofig_status with the session id.
CALL_BODY='{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"turbofig_status","arguments":{}}}'
RESPONSE="$(curl -s \
  -H "${AUTH}" \
  -H "Accept: ${ACCEPT}" \
  -H "Content-Type: application/json" \
  -H "mcp-session-id: ${SESSION_ID}" \
  -d "${CALL_BODY}" \
  "${BASE}")"

# The response is an SSE stream. The data: line carries the JSON-RPC result.
# The tool text is the JSON string {"ok":true}, escaped inside content[0].text.
echo "${RESPONSE}" | grep -q '^data:' || fail "tools/call response was not an SSE stream"
# The tool text is a JSON string, so the braces and quotes are escaped inside
# content[0].text: the literal substring on the wire is \"ok\":true .
echo "${RESPONSE}" | grep -qF '\"ok\":true' || fail "turbofig_status did not return {\"ok\":true}. Body:
${RESPONSE}"

# Step 4: statefulness. A tools/call without the session id must be rejected.
STATUS="$(curl -s -o /dev/null -w '%{http_code}' \
  -H "${AUTH}" \
  -H "Accept: ${ACCEPT}" \
  -H "Content-Type: application/json" \
  -d "${CALL_BODY}" \
  "${BASE}")"
[ "${STATUS}" != "200" ] || fail "tools/call without a session id was not rejected (HTTP ${STATUS})"

echo "PASS: full curl handshake works and turbofig_status returned {\"ok\":true}"
