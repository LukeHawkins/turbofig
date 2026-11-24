# turbofig js vs the old recipe vs raw MCP (2026-11-23)

Mock plugin answers a simple execute job. 50 runs each (3 warm-up runs not counted). Wall time from this parent process, process spawn included. Release build of this checkout. Machine: darwin arm64, bun 1.4.2.

Process start alone (`/usr/bin/true`): median 11.7 ms, p95 24.8 ms.

| Path | median ms | p95 ms |
|---|---|---|
| old recipe, /bin/zsh -c | 241.9 | 514.9 |
| old recipe, /bin/bash -c | 166.9 | 860.3 |
| turbofig js | 74.5 | 103.7 |
| turbofig js via /bin/zsh -c | 95.9 | 167.2 |
| raw MCP tools/call (HTTP, no process spawn) | 0.4 | 0.6 |

Notes:
- The old recipe polls every 20 ms (`sleep .02`), so it adds up to 20 ms of wait plus the process starts of `seq`, `sleep` and `cat`.
- The raw MCP row has no process spawn and keeps one session. It is the transport floor of MCP mode, not what an agent pays per call (an agent adds model turn time and tool schema tokens).
- Re-run: `bun bench/js-vs-recipe.ts [runs]` after `cargo build --release`.
