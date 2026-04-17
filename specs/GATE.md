# Backlot End-to-End Test Harness — Spec

## Purpose

A manually-invoked integration test suite that exercises the backlot crate stack against real LLM providers. Tests the full dependency chain (flick → lot → reel → vault → epic) bottom-up, verifying that the crates interoperate correctly with live models and real OS sandboxing.

This suite is separate from `cargo test` (which uses mocks/stubs). It is expensive to run (real API calls, real tokens) and is invoked on-demand, not in CI.

The harness is a Rust CLI binary called **`gate`**, implemented as a binary-only crate in the workspace (no library component).

## Prior Art

Gate's structure draws on a NuShell-based predecessor at `C:\UnitySrc\rig` that established the bottom-up dependency-chain test ordering, the use of `cargo test` as an oracle for epic, and the convention of recording E2E-discovered issues in a findings tracker.

---

## Design

### Language and Crate Structure

Rust. `gate` is a binary-only crate (no `lib.rs`) in the workspace. Unlike the other backlot crates which are split into library + CLI pairs, gate has no library consumers — it exists solely as a test runner.

```
gate/
  Cargo.toml
  src/
    main.rs           # CLI entry point (clap)
    runner.rs         # Stage orchestration, binary discovery
    check.rs          # Assertion helpers and test result types
    report.rs         # Result collection, summary formatting
    stage/
      mod.rs
      flick.rs        # Stage 1: LLM API calls
      lot.rs          # Stage 2: Sandbox enforcement
      reel.rs         # Stage 3: Agent tool loop
      vault.rs        # Stage 4: Knowledge store
      epic.rs         # Stage 5: Recursive orchestrator
      mech.rs         # Stage 6: Workflow engine (placeholder)
  fixtures/
    flick/            # Config YAMLs, schemas
    lot/              # Policy YAMLs (per-platform)
    reel/             # Agent configs, workspace seed files
    vault/            # Vault configs
    epic/             # Test project templates (Cargo.toml, src/main.rs)
  PLATFORMS.md        # Per-platform validation status
  FINDINGS.md         # E2E-discovered issues
```

The crate depends on `clap` (CLI), `serde`/`serde_json` (output parsing), `tempfile` (scratch directories), and standard library types for subprocess execution (`std::process::Command`). It does **not** depend on any other backlot crate — it exercises them purely through their CLI binaries.

### Binary Discovery

Gate locates the other backlot binaries at runtime. It does not link against their libraries.

**Resolution order:**
1. `--bin-dir <path>` flag (explicit override)
2. Workspace `target/debug/` (default — derived from gate's own executable path or `CARGO_MANIFEST_DIR`)

The runner verifies all required binaries exist before executing any stage. Missing binaries produce an actionable error message naming the missing binary and suggesting `cargo build`.

### Fixture Management

Fixtures are static files committed under `gate/fixtures/`. Gate copies them into per-run scratch directories so tests are isolated and repeatable. Tests never modify committed fixtures.

For tests that need generated content (e.g., epic's git-initialized Rust project), gate generates the project programmatically in a scratch directory at stage setup time.

### CLI Interface

```
gate                             # Run all stages
gate --only flick                # Run one stage
gate --from reel                 # Skip stages before reel
gate --only epic --verbose       # Save session transcripts
gate --bin-dir ./target/release  # Use release binaries
```

**Flags:**
- `--only <stage>` — run exactly one stage
- `--from <stage>` — resume from a stage (skip earlier ones)
- `--verbose` — capture and save LLM session transcripts to output directory; implies `--keep-scratch`
- `--bin-dir <path>` — override binary discovery
- `--timeout <seconds>` — per-stage wall-clock timeout (default: 300s, 600s for epic)
- `--output-dir <path>` — where to write results and transcripts (default: `gate/output/`)
- `--keep-scratch` — preserve scratch directory even on success

**Exit codes:**
- `0` — all tests passed
- `1` — one or more tests failed
- `2` — prerequisite check failed (missing binaries, missing credentials, sandbox not set up)

### Subprocess Execution

Gate runs all crate CLIs via `std::process::Command`. Each invocation captures stdout, stderr, and exit code. A helper wraps this into a structured `CommandResult`:

```rust
struct CommandResult {
    stdout: String,
    stderr: String,
    exit_code: i32,
    duration: Duration,
}
```

Timeout enforcement uses `Command::spawn` + `Child::wait_timeout` (or a thread-based equivalent), killing the child if the deadline expires.

### Assertion and Reporting Model

Each test produces a `TestResult`:

```rust
struct TestResult {
    stage: String,
    test: String,
    outcome: TestOutcome,     // Pass, Fail(String), Skip(String), SoftFail(String)
    duration: Duration,
    cost_usd: Option<f64>,
    tokens_in: Option<u64>,
    tokens_out: Option<u64>,
}
```

`SoftFail` is for infrastructure-dependent tests (e.g., network connectivity) — reported as a warning, does not cause a non-zero exit.

Assertion helpers are functions in `check.rs`:

```rust
fn assert_exit_ok(result: &CommandResult, label: &str) -> Result<(), TestFailure>;
fn assert_exit_fail(result: &CommandResult, label: &str) -> Result<(), TestFailure>;
fn assert_json_field(json: &Value, field: &str, label: &str) -> Result<(), TestFailure>;
fn assert_contains(haystack: &str, needle: &str, label: &str) -> Result<(), TestFailure>;
fn assert_eq<T: PartialEq + Debug>(actual: &T, expected: &T, label: &str) -> Result<(), TestFailure>;
fn assert_path_exists(path: &Path, label: &str) -> Result<(), TestFailure>;
```

Each assertion prints a `PASS:` or `FAIL:` line immediately (for live progress feedback) and returns `Result` so the calling test can early-return on failure.

### Result Summary

After all stages complete (or after a hard failure), gate prints a summary table:

```
╔══════════════════════════════════════════════════════════════╗
║  Gate — End-to-End Results                                   ║
╠══════════════════════════════════════════════════════════════╣
║  Stage    Tests  Pass  Fail  Skip  Cost     Duration         ║
║  flick        6     6     0     0  $0.02    4.2s             ║
║  lot          8     8     0     0  $0.00    6.1s             ║
║  reel         5     5     0     0  $0.08   12.3s             ║
║  vault        5     5     0     0  $0.18   31.5s             ║
║  epic         3     3     0     0  $1.24   85.0s             ║
╠══════════════════════════════════════════════════════════════╣
║  Total       27    27     0     0  $1.52  139.1s             ║
╚══════════════════════════════════════════════════════════════╝
```

When `--verbose` is set, gate also writes `output/results.json` (structured results) and saves captured stdout/stderr for each test to `output/transcripts/`.

### Output Directory

```
gate/output/                   # gitignored
  results.json                 # Structured results from last run
  transcripts/                 # Per-test captured output (--verbose)
    flick-basic-invocation.stdout
    flick-basic-invocation.stderr
    reel-readonly-session.stdout
    ...
```

### Scratch Directories

Lot, reel, and epic stages need filesystem paths to grant sandbox access to. On Windows these must be project-local (the AppContainer ancestor-traverse constraint documented in `CLAUDE.md` rules out system temp).

**Location:** `target/gate-scratch/`. Lives alongside cargo build artifacts; implicitly gitignored (`target/` is in the root `.gitignore`); wiped naturally by `cargo clean`.

**Per-run timestamped subdirectory:**

```
target/gate-scratch/
  run-20260417-143052/
    lot/
    reel/
    vault/
    epic/
```

Each gate invocation gets its own `run-<timestamp>/` directory. Within a run, each stage has its own subdirectory. This isolates concurrent runs and prevents stale state from previous runs from interfering with new ones.

**Lifetime:** delete-on-success, keep-on-failure. On success the entire `run-<timestamp>/` is removed. On failure the directory is preserved and its path is printed in the failure summary. `--keep-scratch` (or `--verbose`) forces preservation even on success.

### Model Aliases

Fixtures reference three tier aliases: **`fast`** (Haiku-class), **`balanced`** (Sonnet-class), **`strong`** (Opus-class). These match the names epic.toml uses for its model tiers, keeping gate consistent with the rest of the stack.

The Stage 0 prerequisite check verifies all three aliases are registered in `~/.flick/models`. The human registers these aliases once during setup, mapping them to whatever concrete models they want.

### Platform Support

Gate supports Windows, macOS, and Linux. Test logic is platform-neutral; platform divergence is isolated to fixture-selection helpers (e.g., `fn lot_policy_path() -> PathBuf` returns a different path per `cfg!(target_os = ...)`), not scattered through test code.

**Per-platform fixtures.** Anything inherently platform-specific lives in per-platform files. Lot policies are committed as `fixtures/lot/policy_windows.yaml`, `policy_macos.yaml`, `policy_linux.yaml`; the fixture loader picks the right one at runtime.

**Platform-aware prerequisite check.** Stage 0 runs `lot setup --check` only on platforms where setup is required and reports which sandbox backend was detected.

**Stage skipping.** When a stage cannot run on the current platform (e.g., epic on a platform without AppContainer support), the stage is skipped and the summary table marks it as `Skip` with a reason — never as `Pass`.

**Validation status** is tracked separately in `gate/PLATFORMS.md`:

| Platform | Status | Notes |
|----------|--------|-------|
| Windows  | TBD    | Primary development platform |
| macOS    | TBD    | Validated locally |
| Linux    | TBD    | Pending Linux environment |

When a platform is validated, status flips to `Pass` with the date and any platform-specific findings recorded.

---

## Stages

### Stage 0: Prerequisites Check

Runs automatically before any test stage. Not a test stage itself — a gate (pun intended) that blocks execution if the environment is not ready.

**Checks:**
- All required binaries exist (`flick`, `lot`, `reel`, `vault`, `epic`, `mech`)
- `~/.flick/providers` exists and is non-empty
- `~/.flick/models` exists and contains aliases `fast`, `balanced`, `strong`
- `lot setup --check` passes (platform sandbox ready)

Failure produces exit code 2 with a message listing exactly what is missing and how to fix it.

### Stage 1: flick — LLM API Calls

**Prerequisites:** Provider(s) registered via `flick provider add` (interactive, one-time).

**Tests:**

| Test | What it verifies |
|------|-----------------|
| `basic-invocation` | Round-trip call via Messages API. Status `complete`, non-empty content, valid usage block, context_hash present. |
| `chatcompletions-invocation` | Same via Chat Completions provider. Verifies second API backend. |
| `tool-declaration-and-resume` | Declare tools → get `tool_calls_pending` → resume with tool results → get `complete`. Tests stateful context round-trip. |
| `structured-output` | `output_schema` produces conformant JSON. Parse and validate required fields. |
| `dry-run` | `--dry-run` returns request payload without API call. Exit 0, non-empty output. |
| `error-invalid-model` | Unknown model name → non-zero exit. |

**Cost:** Low (5–6 single-turn API calls).

### Stage 2: lot — Sandbox Enforcement

**Prerequisites:** `lot setup` run from Administrator terminal (Windows), or equivalent platform setup.

**Tests:**

| Test | What it verifies |
|------|-----------------|
| `probe` | Platform sandbox capabilities detected (AppContainer/seccomp/Seatbelt). |
| `setup-check` | `lot setup --check` passes. Gate test for downstream stages. |
| `fs-read-allowed` | Sandboxed process can read granted paths. |
| `fs-write-allowed` | Sandboxed process can write to granted paths. Verified outside sandbox. |
| `fs-deny-overrides-read` | Explicit deny blocks read even when parent is granted. |
| `network-denied` | Sandbox blocks network when `allow: false`. |
| `network-allowed` | Sandbox permits network when `allow: true`. **Soft-fail** (infrastructure-dependent). |
| `timeout` | Process killed after timeout. Exit code 124. |

**Cost:** Zero (no LLM calls).

### Stage 3: reel — Agent Tool Loop

**Prerequisites:** Stage 1 credentials, Stage 2 sandbox setup.

**Tests:**

| Test | What it verifies |
|------|-----------------|
| `readonly-session` | Agent uses Glob/Read tools on a workspace. Status `Ok`, usage present, content references known files. |
| `write-session` | Agent creates a file. File exists on disk with correct content after session. |
| `nushell-execution` | Agent invokes NuShell tool. Session completes without crash. |
| `multi-turn` | Task requiring multiple tool rounds. Asserts `tool_calls > 1`. |
| `error-invalid-model` | Bad model in config → non-zero exit. |

**Cost:** Moderate (4 multi-turn agent sessions, ~$0.05–0.15).

### Stage 4: vault — Knowledge Store

**Prerequisites:** Stage 1 credentials, Stage 2 sandbox setup.

**Setup:** Clean-slate removal of store directory before stage. Created in the per-run scratch directory.

**Tests:**

| Test | What it verifies |
|------|-----------------|
| `bootstrap` | Pipes requirements → `vault bootstrap`. Raw docs created, derived docs generated, changelog exists. Usage block in output. |
| `record-new` | `vault record --mode new` creates a new raw document. |
| `record-append` | `vault record --mode append` appends to existing document. |
| `query` | `vault query` returns a non-empty answer referencing recorded content. |
| `reorganize` | After multiple records, `vault reorganize` restructures derived docs. Changelog grows. |

**Cost:** Moderate (5 librarian sessions, ~$0.10–0.30).

### Stage 5: epic — Recursive Orchestrator

**Prerequisites:** Stage 1 credentials, Stage 2 sandbox setup, `cargo` and `git` in PATH.

**Setup:** Gate programmatically creates a minimal Rust project with a known bug in a scratch directory, initializes a git repo, and writes `epic.toml`.

**Tests:**

| Test | What it verifies |
|------|-----------------|
| `leaf-task` | Epic fixes a deliberate off-by-one bug. Oracle: `cargo test` passes post-run. State file exists. |
| `status` | `epic status` reports on completed run. Exit 0. |
| `resume-completed` | `epic resume` on an already-completed run exits cleanly (exit 0, no re-execution). |

Epic tests assert outcomes (does `cargo test` pass?), not behavior (which tools did the LLM call?). LLM behavior is non-deterministic; verification gates are deterministic.

**Cost:** Highest (full orchestration cycles, ~$0.50–2.00).

### Stage 6: mech — Workflow Engine (Placeholder)

A `stage/mech.rs` module is present so the orchestrator's stage enum is complete and adding tests later is purely additive. The module registers the stage, prints `mech: stage placeholder — no tests defined yet`, and returns success without running anything. Stage 0 verifies the `mech` binary exists.

Tests are deferred until mech-cli supports real workflow execution (see [Future Work](#future-work)). Planned tests:

- `single-prompt` — trivial workflow with one prompt block + structured output schema. Asserts workflow completes, output matches schema, usage reported.
- `multi-block-transitions` — 2–3 block workflow with CEL guards. Asserts expected execution order, state propagation, final output.
- `call-block-with-agent` — workflow with a call block invoking a reel agent. Asserts agent runs in sandbox and result flows back into workflow scope.

---

## Prerequisites and Setup

### One-Time Setup (Human)

1. **Build the workspace:** `cargo build` from workspace root (debug or release). This produces all binaries including `gate` itself.
2. **Sandbox setup (Windows):** Run `lot setup` and `reel setup` from an Administrator terminal.
3. **Provider registration:** `flick provider add <name>` for each LLM provider (interactive — requires typing API keys).
4. **Model alias registration:** `flick model add` for each of `fast`, `balanced`, `strong`, or write `~/.flick/models` TOML directly.

### Per-Run (Automated)

Gate discovers binaries automatically from the workspace target directory. The Stage 0 prerequisite check validates everything before spending any tokens.

---

## Cost Management

- **Budget display:** The summary always shows total cost. Individual test costs shown when `--verbose`.
- **Stage ordering:** Cheapest stages first (lot=free, flick=cheap) → expensive stages last (epic). Early failures avoid wasting budget.
- **Model tiering:** Tests use the cheapest model that can reliably pass. Vault query/record use `fast`; only bootstrap/reorganize use `balanced`. Epic tests use small, well-defined tasks to minimize token spend.
- **No CI integration:** This suite is never run automatically.

---

## Findings Tracker

When the E2E suite reveals bugs or issues in the crates, they are recorded in `gate/FINDINGS.md`. Each finding has:

- **ID:** `F-NNN`
- **Crate:** Which crate is affected
- **Description:** What was observed
- **Status:** OPEN / RESOLVED (with commit ref)
- **Workaround:** If any

E2E testing against real models reliably surfaces issues that unit tests miss.

---

## Workspace Integration

Add to workspace `Cargo.toml`:

```toml
[workspace]
members = [
    # ... existing members ...
    "gate",
]
```

Gate is excluded from CI test runs (it requires real credentials and costs money). The workspace `cargo test` runs only unit/integration tests for the other crates. Gate is built by `cargo build` but only executed manually.

---

## Future Work

- **`gate.toml` configuration file** at the workspace root, allowing per-stage overrides of model alias mappings and other run-time defaults. Fixtures continue to reference tier names (`fast`/`balanced`/`strong`); gate substitutes them at fixture-load time.
- **Stage 6 (mech) tests.** Blocked on mech-cli wiring a real `AgentExecutor` (currently uses a `StubAgent` that errors for any workflow with `prompt` or `call` blocks). Tracked as a mech-cli work item.
