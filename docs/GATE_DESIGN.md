# Gate — End-to-End Test Harness Design

Gate is a manually-invoked integration test suite that exercises the backlot crate stack against real LLM providers and real OS sandboxing. It is **separate** from `cargo test` (which uses mocks/stubs), is expensive to run (real API calls, real tokens), and is invoked on-demand — never in CI.

Gate is implemented as a **binary-only crate** in the workspace (no `lib.rs`). It does not link against any other backlot crate; it exercises them purely through their CLI binaries via `std::process::Command`.

---

## Crate Structure

```
gate/
  Cargo.toml
  src/
    main.rs           # CLI entry point (clap), config plumbing
    types.rs          # Shared types: Stage, TestOutcome, *Result, GateConfig
    check.rs          # Assertion helpers (PASS/FAIL printers, TestFailure)
    report.rs         # Summary table + JSON output
    exec.rs           # Subprocess execution with timeout
    scratch.rs        # Per-run scratch dir lifecycle
    # later deliverables add: runner.rs, stage/
```

Direct dependencies: `clap`, `serde`, `serde_json`, `tempfile`. No other backlot crates.

---

## Shared Types (`types.rs`)

### `Stage`

```rust
pub enum Stage { Flick, Lot, Reel, Vault, Epic, Mech }
```

Implements `Display` (lowercase: `flick`, `lot`, ...), `FromStr` (lowercase only — rejects empty, uppercase, unknown), `Copy`, `Eq`, `Ord`. The `Ord` impl matches dependency order, which lets `--from <stage>` filter via `>=`.

- `Stage::all()` → `[Flick, Lot, Reel, Vault, Epic, Mech]` in dependency order.
- `Stage::default_timeout()` → `Duration` per stage. Epic = 600s, all others = 300s.

### `TestOutcome`

```rust
pub enum TestOutcome { Pass, Fail(String), Skip(String), SoftFail(String) }
```

`SoftFail` is for infrastructure-dependent tests (e.g., network connectivity) — reported as a warning but does not cause a non-zero exit. Hence `is_failure()` is true only for `Fail`, never `SoftFail`.

### `TestResult`, `StageResult`, `CommandResult`

Plain data structs. `StageResult::all_passed()` is true when zero `Fail` outcomes are present (so a stage with only soft-fails still passes the suite). `total_cost()` sums `cost_usd` across results, ignoring `None`.

### `GateConfig`

Run-time configuration parsed from CLI flags:

| Field | Type | Default |
|-------|------|---------|
| `only` | `Option<Stage>` | `None` |
| `from` | `Option<Stage>` | `None` |
| `verbose` | `bool` | `false` |
| `bin_dir` | `Option<PathBuf>` | `None` |
| `timeout` | `Option<Duration>` | `None` |
| `output_dir` | `PathBuf` | `gate/output/` |
| `keep_scratch` | `bool` | `false` |

Three derived helpers centralise policy that downstream modules share:

- `effective_timeout(stage)` — explicit `timeout` if set, else `stage.default_timeout()`.
- `should_run(stage)` — respects `only` (exclusive) and `from` (inclusive lower bound). With neither set, all stages run.
- `effective_keep_scratch()` — true if `keep_scratch` **or** `verbose`. `--verbose` implies preserving the scratch tree because it is needed to inspect saved transcripts.

---

## Subprocess Execution (`exec.rs`)

`run_command(program, args, working_dir, env, timeout) -> io::Result<CommandResult>` is the single entry point through which gate stages spawn binaries. It captures stdout, stderr, exit code, and wall-clock duration into a `CommandResult` and enforces a wall-clock timeout.

**Pipe draining.** Stdout and stderr are read on background threads from the moment the child is spawned. A naive `wait()`-then-read scheme deadlocks once a chatty child fills its pipe buffer; the per-pipe drain threads keep the child unblocked regardless of output volume.

**Timeout enforcement.** A poll loop calls `Child::try_wait()` every 25 ms. When `start.elapsed() >= timeout` the child is killed and reaped, the returned `exit_code` is set to `TIMEOUT_EXIT_CODE` (`-1`, deliberately negative so it cannot collide with a legitimate platform exit code), and a `gate: command timed out after Xs` line is appended to stderr.

**Error model.** `Err` is reserved for genuine I/O failures (spawning a missing binary, OS-level wait failure). Non-zero exit codes are normal program output and surface through `CommandResult::exit_code`, never as `Err` — letting stages distinguish "tool ran and reported failure" from "tool could not be invoked at all".

**Environment.** The `env` slice augments (and overrides) the inherited parent environment; it does not clear it. Tests rely on `PATH` being inherited so `git`, `sh`, `cmd`, `ping` resolve.

## Scratch Directories (`scratch.rs`)

`scratch_base() -> PathBuf` resolves to `<workspace>/target/gate-scratch/`, derived from `CARGO_MANIFEST_DIR`. Living under the workspace `target/` keeps scratch paths project-local and out of `%TEMP%` / `C:\Users` — the workspace `CLAUDE.md` rule that `lot`/`reel`/`epic` sandbox-granted paths must avoid system temp (Windows AppContainer ancestor-traverse ACEs cannot be granted there).

`create_run_dir() -> io::Result<PathBuf>` creates a `run-YYYYMMDD-HHMMSS/` directory plus `lot/`, `reel/`, `vault/`, `epic/` subdirs (eagerly, even for stages that may not run). The timestamp is computed in UTC with no external date crate dependency, using Howard Hinnant's `civil_from_days` algorithm.

**Race-safe naming.** The implementation does not check `exists()` then `mkdir`; that pattern loses to concurrent callers within the same one-second timestamp bucket (parallel test threads, parallel gate invocations). Instead `create_dir` is invoked directly and `AlreadyExists` triggers a `-1`, `-2`, ... suffix retry until a fresh name succeeds.

`cleanup_run_dir(path)` is a recursive `remove_dir_all` that swallows `NotFound` so the runner can call it unconditionally on success without a stat-then-delete dance.


---

## CLI (`main.rs`)

A clap-derive `Cli` struct mirrors `GateConfig`. `--only` and `--from` are mutually exclusive (clap `conflicts_with`). `Stage` values are parsed through its `FromStr` impl, so unknown values produce a `ValueValidation` error rather than the generic `InvalidValue` from `value_enum`.

`Cli::into_config()` is the single conversion point that produces a `GateConfig`. The runner module (D5) consumes it; D1's `main` parses, builds the config, prints `gate: not yet implemented`, and exits 0.

### Exit codes (planned, wired in later deliverables)

- `0` — all tests passed
- `1` — one or more tests failed
- `2` — prerequisite check failed (missing binaries/credentials/sandbox setup)

---

## Design Notes

**Dead-code allowance.** `types.rs` carries `#![allow(dead_code)]` because the struct fields and helper methods are exercised by tests in this deliverable but only consumed by production code in later deliverables (D2+). This keeps each deliverable's diff focused on its own scope without forcing premature wiring.

**Binary-only crate.** Unlike the other backlot crates which split library + CLI, gate has no library consumers. The types live next to `main.rs` and are reached via `mod types;`.

**Workspace lints.** Gate inherits `[lints] workspace = true`, which denies `unsafe_code` and `clippy::all`. CI does not run gate, but `cargo build -p gate` and `cargo clippy -p gate` are expected to be clean.

**Future scope** (subsequent deliverables): runner with binary discovery, per-stage modules (`flick`, `lot`, `reel`, `vault`, `epic`, `mech`), and the Stage 0 prerequisite check. See `specs/GATE.md` for the full system design.
