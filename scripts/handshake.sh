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

# Use a dedicated test port so the script never clashes with a real daemon on
# the default 3846. Override with TURBOFIG_MCP_PORT if needed.
PORT="${TURBOFIG_MCP_PORT:-38460}"
BASE="http://127.0.0.1:${PORT}/mcp"
ACCEPT="application/json, text/event-stream"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${ROOT}/target/debug/turbofig-mcp"

fail() {
  echo "FAIL: $1" >&2
  exit 1
}

# Build the daemon if the binary is missing.
if [ ! -x "${BIN}" ]; then
  echo "Building the daemon..."
  (cd "${ROOT}" && cargo build --quiet)
fi

# Start the daemon in the background.
TURBOFIG_MCP_PORT="${PORT}" "${BIN}" &
DAEMON_PID=$!

# Always stop the daemon on exit.
cleanup() {
  kill "${DAEMON_PID}" 2>/dev/null || true
  wait "${DAEMON_PID}" 2>/dev/null || true
}
trap cleanup EXIT

# Wait for the daemon to accept connections (up to 5 seconds).
for _ in $(seq 1 50); do
  if curl -s -o /dev/null "http://127.0.0.1:${PORT}/mcp" 2>/dev/null; then
    break
  fi
  sleep 0.1
done

HEADERS="$(mktemp)"
trap 'rm -f "${HEADERS}"; cleanup' EXIT

# Step 1: initialize. Capture the response headers to read mcp-session-id.
INIT_BODY='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","clientInfo":{"name":"curl-skill","version":"0.1.0"},"capabilities":{}}}'
curl -s -D "${HEADERS}" -o /dev/null \
  -H "Accept: ${ACCEPT}" \
  -H "Content-Type: application/json" \
  -d "${INIT_BODY}" \
  "${BASE}"

# Read the session id from the response headers (case-insensitive match).
SESSION_ID="$(grep -i '^mcp-session-id:' "${HEADERS}" | head -1 | sed 's/^[^:]*:[[:space:]]*//' | tr -d '\r\n')"
[ -n "${SESSION_ID}" ] || fail "initialize did not return an mcp-session-id header"
echo "Got session id: ${SESSION_ID}"

# Step 2: notifications/initialized with the session id.
NOTIF_BODY='{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}'
curl -s -o /dev/null \
  -H "Accept: ${ACCEPT}" \
  -H "Content-Type: application/json" \
  -H "mcp-session-id: ${SESSION_ID}" \
  -d "${NOTIF_BODY}" \
  "${BASE}"

# Step 3: tools/call turbofig_status with the session id.
CALL_BODY='{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"turbofig_status","arguments":{}}}'
RESPONSE="$(curl -s \
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
  -H "Accept: ${ACCEPT}" \
  -H "Content-Type: application/json" \
  -d "${CALL_BODY}" \
  "${BASE}")"
[ "${STATUS}" != "200" ] || fail "tools/call without a session id was not rejected (HTTP ${STATUS})"

echo "PASS: full curl handshake works and turbofig_status returned {\"ok\":true}"
