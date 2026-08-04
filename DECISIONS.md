# Decisions

1. **Eval-first, ~4 tools (turbofig_execute / turbofig_get_selection / turbofig_screenshot / turbofig_status).**
   Why: token economy (~96% less tool-def load), resilience (new Figma APIs work same-day), full capability through one tool. Baseline competitor figma-console-mcp already has eval + multi-file; we win on DX, performance, Rust single-binary, and true always-on.

2. **Rust daemon via rmcp 3.1.0 (transport-streamable-http-server) + legacy_session_mode.**
   Why: client uses 2025-03-26 spec with mcp-session-id; rmcp defaults to the session-less 2026-07-28 spec. `legacy_session_mode: true` on `StreamableHttpServerConfig` with `LocalSessionManager` makes rmcp issue the `mcp-session-id` header on `initialize` and require it on subsequent calls.
   Phase 1 result: CONFIRMED. `initialize` returns `mcp-session-id`; subsequent calls reuse it; responses are SSE `data:` lines. A Rust integration test (`daemon/tests/transport.rs`) and a curl script (`scripts/handshake.sh`) both pass. The rmcp #1108 risk is retired.
   Fallback (retained, not needed now): if a future rmcp release breaks `legacy_session_mode`, hand-roll the transport with axum. Add a POST `/mcp` handler that assigns an `mcp-session-id` on `initialize`, stores session state in an in-memory map, and streams SSE `data:` responses. The WebSocket connection to the plugin stays on axum regardless. This is a drop-in replacement because the daemon already owns the axum `Router`.

3. **Ports are a product contract: default HTTP 3846, WS 3847, env-overridable (TURBOFIG_MCP_PORT / TURBOFIG_WS_PORT).**
   Why: intentionally overrides ai-boilerplate's "randomised high ports" rule because 3846 is a drop-in for existing curl-based skills.

4. **eval blocks a Figma Community store listing; ship as dev-install.**
   Why: review rejects arbitrary server-run code. Keep the plugin hybrid-ready: a {type} dispatch table so a community-safe command vocabulary build is additive later with no rework.

5. **Distribution: primary `npx turbofig-mcp` using per-platform binary packages as optionalDependencies (the esbuild/Biome pattern).**
   Why: no postinstall network fetch, works on locked-down corporate networks. Secondary: cargo install, Homebrew tap, GitHub Release binaries + curl|sh. Daemon nudges on a newer version; plugin warns on version mismatch.

6. **Trainability: ship a generic good-designer taste baseline; user brand/project packs load on top and switch at runtime.**
   Why: the public build ships only the generic baseline, never private packs.

7. **Token strategy: subagent context firewall (screenshots live in disposable subagents), downscaled + milestone-only screenshots, shaped ids-first returns, batched execute.**
   Why: the whole product pitch is token economy. Every design decision is judged on token cost.

8. **Multi-file: true routing by fileKey (N independent sessions <-> N files), stronger than console-mcp's broadcast execute_across_files.**
   Why: per-session isolation is a hard requirement for concurrent multi-file design work.

9. **Ship gate placement: Phase 5 is a provisional baseline only. The binding "far better than console-mcp" gate runs after Phase 7.**
   Why: the token and speed wins come from the helper library, the design-worker orchestration, and the subagent firewall added in Phases 6 and 7. Gating at Phase 5 would measure the system without its main levers.

10. **Context firewall is enforced, not just convention. Inline screenshots need an explicit opt-in and warn past a budget. File-mode plus subagent-read is the default path. Large reads without depth or fields are capped or warned.**
    Why: for a redistributed product the token promise cannot depend on client prompt discipline alone.

11. **Brand packs bind per session, keyed like the routing registry.**
    Why: concurrent multi-file sessions may need different brands at once, so a single global active pack would break isolation.

12. **Daemon lifecycle: launchd KeepAlive restarts the daemon on crash. Every call has a daemon-side timeout so a silent plugin never hangs a request. On restart the in-memory session registry is lost, so clients get a clear reinitialize signal rather than a silent error.**
    Why: always-on reliability requires automatic restart and clean failure signalling.

13. **eval security for redistribution: the daemon binds to 127.0.0.1 only. eval has resource guards beyond the timeout (runaway loop and memory). The prompt-injection and file-exfiltration risk is documented, because arbitrary eval on a user's open file is powerful. Acceptable for local use, called out for redistributed use.**
    Why: local-only binding limits the attack surface; documented risk keeps redistributors informed.
    DNS-rebinding defence (Phase 1, deliberate): rmcp `StreamableHttpServerConfig` defaults `allowed_hosts` to `["localhost", "127.0.0.1", "::1"]`. This rejects a spoofed `Host` header with 403, so a browser page on a rebound domain cannot reach the loopback endpoint. We keep this default on purpose. We do NOT narrow it to `127.0.0.1:<port>` because curl skills may use `localhost`. `allowed_origins` stays empty (Origin validation skipped) because there is no browser client and the `Host` allow-list already blocks the browser attack path. Revisit `allowed_origins` if a browser client is ever added.

14. **Long-job durability: the daemon runs decoupled from any client session, so a session end or client crash never kills an in-flight job. Long design jobs checkpoint the plan spec and per-section progress to disk, and resume from the last completed section. Operations are idempotent, keyed by stable node id, so a resume never duplicates.**
    Why: the current supergateway stack dies with the session (SIGTERM), leaving big jobs half done. Decoupling plus checkpoint and resume lets a huge job survive a restart with completed work intact.
