# Turbofig Benchmark Harness

This harness measures the transport cost (approximate tokens in and out) and wall-time of scripted design jobs driven through the file-bridge.

## What the harness measures

- **Tokens in**: token count of each job payload sent to the daemon (approximate: `ceil(chars / 4)`).
- **Tokens out**: token count of the JSON result returned by the daemon.
- **Wall-time**: elapsed milliseconds from writing the inbox file to reading the outbox file.

The file-bridge is the mechanism under measurement. The harness does not measure Figma rendering time or network latency beyond the daemon.

## Scenarios

| Scenario | Jobs | Description |
|----------|------|-------------|
| `webpage` | 5 | Marketing page: page setup, nav, hero, feature grid, footer. |
| `deck20` | 3 | 20-slide deck built in three batched execute calls. |

## How to run (dry-run, no daemon required)

```sh
bun bench/harness.ts --dry-run
bun bench/harness.ts --dry-run --scenario webpage
bun bench/harness.ts --dry-run --scenario deck20 --out report.json
```

The `--dry-run` flag uses a stub transport that returns a canned result without writing to disk or contacting the daemon. Use it to verify the harness and inspect token counts.

## How to run live (daemon must be running)

Start the daemon and open a Figma file with the plugin connected. Then run:

```sh
bun bench/harness.ts --scenario webpage
bun bench/harness.ts --scenario all --out results.json
bun bench/harness.ts --scenario deck20 --bridge-dir /custom/path
```

The harness writes each job to `~/.turbofig/inbox/<id>.json` and polls `~/.turbofig/outbox/<id>.json` for the result.

## Baseline comparison

Save a baseline with `--out`:

```sh
bun bench/harness.ts --dry-run --scenario webpage --out baseline.json
```

Compare a later run against it with `--baseline`:

```sh
bun bench/harness.ts --dry-run --scenario webpage --baseline baseline.json
```

The harness prints the ratio `this run / baseline` for total tokens. A ratio below 1.0 means the new run used fewer tokens. This is the seam Phase 5 uses for its provisional read comparison.

## CLI options

| Option | Default | Description |
|--------|---------|-------------|
| `--scenario <name\|all>` | `all` | Scenario to run: `webpage`, `deck20`, or `all`. |
| `--bridge-dir <path>` | `~/.turbofig` | Override the bridge directory. |
| `--out <file.json>` | (none) | Write the full JSON report to this file. |
| `--dry-run` | false | Use a stub transport. No daemon required. |
| `--baseline <file.json>` | (none) | Compare total tokens against a saved baseline. |

## Running the tests

```sh
bun test bench/harness.test.ts
```
