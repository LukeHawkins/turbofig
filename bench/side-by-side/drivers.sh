#!/usr/bin/env bash
# Shared helper functions for the untimed side of the benchmark: creating and
# deleting pages, running verification reads, and taking screenshots. These
# never run inside a timed claude -p session; they are the harness's own
# direct calls over each stack's channel.
set -euo pipefail

TF_FILEKEY="ZdDvYYjKvLJEjXde6acpWE"
CM_SESSION_FILE="/tmp/cm-session-id.txt"
CM_URL="http://127.0.0.1:3846/mcp"
CM_REQ_ID_FILE="/tmp/cm-req-id.txt"

# ---------- turbofig (file bridge) ----------

tf_exec() {
  local code="$1"
  local id
  id=$(uuidgen)
  jq -n --arg op "execute" --arg code "$code" --arg fk "$TF_FILEKEY" \
    '{op:$op, code:$code, fileKey:$fk}' > ~/.turbofig/inbox/"$id".json
  for _ in $(seq 1 60); do
    if [ -f ~/.turbofig/outbox/"$id".json ]; then
      cat ~/.turbofig/outbox/"$id".json
      rm -f ~/.turbofig/outbox/"$id".json
      return 0
    fi
    sleep 0.25
  done
  echo '{"ok":false,"code":"timeout_harness"}'
  return 1
}

tf_new_page() {
  local name="$1"
  tf_exec "const p = figma.createPage(); p.name = '${name}'; figma.currentPage = p; return p.id;"
}

tf_delete_page_by_name() {
  local name="$1"
  tf_exec "await figma.loadAllPagesAsync(); const p = figma.root.children.find(n => n.type === 'PAGE' && n.name === '${name}'); if (p) { p.remove(); return 'deleted'; } return 'not_found';"
}

tf_select_page() {
  local name="$1"
  tf_exec "await figma.loadAllPagesAsync(); const p = figma.root.children.find(n => n.type === 'PAGE' && n.name === '${name}'); if (p) { figma.currentPage = p; return p.id; } return null;"
}

# ---------- console-mcp (MCP HTTP via curl) ----------

cm_init_session() {
  local resp sid
  resp=$(curl -sS -m 10 -D - "$CM_URL" \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"bench-harness","version":"1.0.0"}}}')
  sid=$(echo "$resp" | grep -i "mcp-session-id" | head -1 | sed 's/.*: //' | tr -d '\r')
  echo "$sid" > "$CM_SESSION_FILE"
  echo "1" > "$CM_REQ_ID_FILE"
  echo "$sid"
}

cm_next_id() {
  local n
  n=$(cat "$CM_REQ_ID_FILE" 2>/dev/null || echo 0)
  n=$((n + 1))
  echo "$n" > "$CM_REQ_ID_FILE"
  echo "$n"
}

cm_call_raw() {
  # $1 = full jsonrpc body. Strips the SSE envelope and prints only the
  # last "data: {...}" JSON payload.
  local sid
  sid=$(cat "$CM_SESSION_FILE")
  curl -sS -m 30 "$CM_URL" \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    -H "mcp-session-id: $sid" \
    -d "$1" | grep '^data: ' | sed 's/^data: //' | tail -1
}

cm_tool_call() {
  # $1 = tool name, $2 = arguments JSON object (as compact JSON string)
  local reqid body
  reqid=$(cm_next_id)
  body=$(jq -n --argjson id "$reqid" --arg name "$1" --argjson args "$2" \
    '{jsonrpc:"2.0", id:$id, method:"tools/call", params:{name:$name, arguments:$args}}')
  cm_call_raw "$body"
}

cm_exec() {
  local code="$1"
  cm_tool_call "figma_execute" "$(jq -n --arg code "$code" '{code:$code}')"
}

cm_new_page() {
  local name="$1"
  cm_exec "const p = figma.createPage(); p.name = '${name}'; figma.currentPage = p; return p.id;"
}

cm_delete_page_by_name() {
  local name="$1"
  cm_exec "await figma.loadAllPagesAsync(); const p = figma.root.children.find(n => n.type === 'PAGE' && n.name === '${name}'); if (p) { p.remove(); return 'deleted'; } return 'not_found';"
}

cm_select_page() {
  local name="$1"
  cm_exec "await figma.loadAllPagesAsync(); const p = figma.root.children.find(n => n.type === 'PAGE' && n.name === '${name}'); if (p) { figma.currentPage = p; return p.id; } return null;"
}
