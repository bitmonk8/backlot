# gate -- Design Notes

This document covers internal architecture and design decisions. End-user
documentation is in [`README.md`](README.md); the original product spec is
in [`specs/GATE.md`](../specs/GATE.md).

## Module map

| Module      | Responsibility |
|-------------|----------------|
| `main.rs`   | Clap CLI parsing; hands off to `runner::run` |
| `types.rs`  | `Stage`, `GateConfig`, `TestOutcome`, `TestResult`, `StageResult`, `CommandResult` |
| `check.rs`  | Assertion helpers (`assert_exit_ok`, `assert_json_field`, ...) returning `Result<(), TestFailure>` |
| `exec.rs`   | `run_command` -- subprocess invocation with wall-clock timeout, captured stdio, drain threads |
| `scratch.rs`| Per-run timestamped scratch trees under `target/gate-scratch/` |
| `report.rs` | Summary table formatting + `results.json` serialization |
| `runner.rs` | Binary discovery + stage orchestration loop |
| `stage/`    | One module per stage; each exposes `pub fn run(&StageContext) -> Vec<TestResult>` |
| `prereqs.rs`| Stage 0 prerequisite checks; runs before any stage and aborts with exit 2 on failure |
| `fixtures/` | Committed YAML/JSON inputs each stage hands to its CLI (per-platform subtree under `lot/`) |

## Stage runner

`runner::run` is the single CLI entry point:

1. Discover binaries (`discover_binaries`); fail with exit code `2` on miss.
2. Create the per-run scratch tree (`scratch::create_run_dir`); fail with `2`.
3. Iterate `Stage::all()` in dependency order, skipping any not selected by
   `GateConfig::should_run`.
4. For each selected stage: build a `StageContext` (cloned binaries,
   cloned config, per-stage scratch subdir, output dir), invoke the stage
   function, time it, collect a `StageResult`. A `Fail` does not abort --
   later stages still run so the operator sees the full picture.
5. Print the summary table (always).
6. When `--verbose`, write `<output_dir>/results.json`. Transcripts are deferred to a later deliverable.
7. Clean up the scratch tree unless `--keep-scratch` / `--verbose` is set,
   or any stage hard-failed. Failures preserve the scratch tree and print
   its path so the operator can inspect.
8. Return exit code: `0` if no hard failures and no `--verbose` output-write failure; `1` if any stage hard-failed; `2` on prerequisite failure (binary discovery, scratch creation) or `--verbose` output-write failure when no stage hard-failed.

### Testability seam

`run_inner` takes the stage runner as a generic `FnMut(Stage,
&StageContext) -> Vec<TestResult>` so unit tests can inject closures with
captured state (e.g., recording the order stages were called, asserting
the per-stage scratch dir exists when the stage runs). The production
`run` wires the closure to a `match`-based `dispatch_stage` registry that
returns the real `crate::stage::*::run` function pointers.

The public `StageFn = fn(&StageContext) -> Vec<TestResult>` alias
documents the contract every stage module honors.

### Binary discovery

`discover_binaries` iterates `Stage::all()` and uses `binary_filename(&stage.to_string())` for each, collecting either a `BinaryPaths` or a `DiscoveryError` whose `missing` field lists *every* absent binary. The single-pass design avoids the
discover-build-rerun loop a fail-on-first design would create.

Default search dir resolution (when `--bin-dir` is absent): take
`current_exe().parent()`. When that parent ends in `deps` -- the layout
`cargo test` uses -- strip one more level so the resolved directory
matches the workspace `target/<profile>/` location where the production
binaries live. Fallback to `CARGO_MANIFEST_DIR/../target/debug` if
`current_exe` is unavailable (rare; some embedded runtimes).

`binary_filename` centralizes the Windows `.exe` suffix so callers never
hand-write platform-conditional names.

### Scratch directories

Project-local under `target/gate-scratch/`, never system temp. The
workspace `CLAUDE.md` rule about Windows AppContainer ancestor-traverse
ACEs makes this a hard requirement -- crates that drive `lot`, `reel`,
or `epic` cannot grant sandbox access to a directory under `C:\Users`.

The runner pre-creates per-stage subdirectories whose names match
`Stage::to_string()`. `create_dir_all` is called defensively at stage
dispatch time so the `flick` and `mech` stages (which `create_run_dir`
does not pre-populate) still observe an existing scratch path.

## Reporting

`report::format_summary` produces a fixed-width box-drawn table; column
widths are constants so the layout is stable across runs. `-0.0` is
normalized to `+0.0` everywhere a cost is rendered, so the formatted
output never carries an accidental sign on empty inputs.

`report::write_results_json` serializes the same data as JSON; the
schema is exercised end-to-end by `report::tests::json_round_trip` and
related tests.

## Conventions

- Tests that need scratch dirs use `target/gate-scratch/<purpose>-tests/`,
  not `tempfile::TempDir::new()`. The latter places the dir under system
  temp, violating the AppContainer constraint.
- Tests must never silently skip. Use `assert!` / `panic!` to fail
  loudly; an early `return` that reports success when nothing was
  verified is a lie (workspace `CLAUDE.md` rule).
- No `unsafe_code`; workspace `[lints]` is `clippy::all = "deny"`.

## Stage 0: prerequisite check (`prereqs`)

Runs in `runner::run` between scratch-dir creation and the stage loop.
Calls `prereqs::check_prerequisites` with the resolved `BinaryPaths`;
on `Err` prints every problem and exits with code `2`. The stage
runner is never invoked, no per-stage scratch subdir is touched, the
results.json file is never written.

Internally `check_prerequisites_inner` takes injectable parameters for
the providers/models directory paths and the `lot setup --check`
result. The unit tests exercise the aggregation logic against project-
local temp directories without ever spawning the real lot binary; the
production wrapper simply binds those parameters to `~/.flick/...` and
the real `lot setup --check` invocation.

Aggregation contract: every check runs unconditionally, every problem
is appended to one `Vec<String>`, and the function returns
`Err(PrereqError)` only at the end. This avoids the discover-fix-rerun
loop a fail-on-first design would create.

## Stage modules

### Stage 1: `flick`

Six tests (`basic-invocation`, `chatcompletions-invocation`,
`tool-declaration-and-resume`, `structured-output`, `dry-run`,
`error-invalid-model`) each invoke `flick run --config <yaml>` with a
committed fixture from `gate/fixtures/flick/`. The model alias `fast`
is used everywhere except `chatcompletions-invocation` (which uses
`balanced` to exercise a second provider/API backend) and
`error-invalid-model` (which references a deliberately-unregistered
alias).

JSON parsing is centralized in `parse_result`; per-test functions
assert on the `status`, `content` array shape, and `usage` block.
The `tool-declaration-and-resume` test rounds through two flick
invocations in the same scratch dir: the first yields a
`tool_calls_pending` result, the second resumes via the
`context_hash` and a synthetic `tool-results.json` file, expecting
`status=complete`. The token/cost figures are summed across both
calls so the summary table reports the whole round-trip cost.

### Stage 2: `lot`

Eight tests covering capability detection (`probe`, `setup-check`),
filesystem enforcement (`fs-read-allowed`, `fs-write-allowed`,
`fs-deny-overrides-read`), network enforcement (`network-denied`,
`network-allowed`), and timeout (`timeout`).

Per-platform fixtures live under `gate/fixtures/lot/<platform>/<test>.yaml`.
The fixture loader `lot_policy_path(name)` selects the right path
based on `cfg!(target_os)`; the `platform_fixture_selection` unit
test verifies the dispatch.

Policy YAMLs reference `${GATE_SCRATCH}` so they can target the
per-stage scratch dir without hard-coded paths. The placeholder is
expanded by lot itself during config load (its built-in
`${VAR}` mechanism); gate sets `GATE_SCRATCH` on the `run_command`
invocation so the lookup resolves before the sandbox is built.

`network-allowed` is the only test that returns
`TestOutcome::SoftFail` when the outbound connection fails: a
firewall-blocked packet does not indicate a sandbox defect, only
that the network is unreachable. `network-denied`, by contrast,
asserts the connection **must** fail.

The `timeout` test pins exit code to exactly `124` (the conventional
`timeout(1)` value lot's CLI uses) **and** verifies the wall-clock
elapsed time exceeds the configured `--timeout`; this makes a child
that crashed for an unrelated reason unable to fool the assertion.

## Future work (D7-D8)

`reel`, `vault`, `epic`, and `mech` stage modules currently return
empty vecs. D7-D8 fill them in:

- D7: `reel`, `vault`
- D8: `epic`, `mech`

The runner framework, prereqs check, binary discovery, scratch
management, and reporting stay unchanged through those deliverables.
