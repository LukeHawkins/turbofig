# Reliability and retries

- **`turbofig mcp` restarts the daemon if it is not reachable.** Your agent's
  MCP client starts `turbofig mcp` on demand; it starts the daemon too, if
  needed. With autostart on, launchd also restarts the daemon after a crash,
  running with `KeepAlive`, so a crash is followed by a restart, not a dead
  daemon. A clean `turbofig stop` is not a crash: the daemon stays stopped
  until the next login, or until `turbofig start` is run.
- **A job in flight when the plugin disconnects, or that times out, may
  still be running.** A retry of that job is not idempotent: the first
  attempt can still complete in Figma after you retry.
- **"Expired in the queue; the job did not run" is safe to retry.** This
  reply means the job's deadline passed before it started, so nothing ran.
- **Use a unique job id for every job.** Reusing an id while the first job
  with that id may still be running does not get you a result; see
  `skills/file-bridge.md`.
