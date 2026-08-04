# Decisions

1. **Eval-first, ~4 tools (turbofig_execute / get_selection / screenshot / status).**
   Why: token economy (~96% less tool-def load), resilience (new Figma APIs work same-day), full capability through one tool. Baseline competitor figma-console-mcp already has eval + multi-file; we win on DX, performance, Rust single-binary, and true always-on.

2. **Rust daemon via rmcp 3.1.0 (transport-streamable-http-server) + legacy_session_mode.**
   Why: client uses 2025-03-26 spec with mcp-session-id; rmcp defaults to the session-less 2026-07-28 spec. Watch rmcp issue #1108; prove the initialize + mcp-session-id round-trip on day one. Fallback: hand-roll with axum SSE + WS.

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
