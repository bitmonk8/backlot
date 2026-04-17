# gate

End-to-end test harness for the backlot workspace.

`gate` is a binary-only crate that exercises the other backlot CLIs
(`flick`, `lot`, `reel`, `vault`, `epic`, `mech`) end-to-end against real
LLM providers and real OS sandboxing. It is **not** part of `cargo test`:
runs cost real money and require provider credentials, so the harness is
invoked manually.

Full design: [`specs/GATE.md`](../specs/GATE.md).

## Status

D1-D6 complete:

- D1-D5: types, assertions, reporting, subprocess execution, scratch
  directories, binary discovery, and the stage runner skeleton.
- D6: Stage 0 prerequisite check (`prereqs`), Stage 1 (`flick`, six
  tests) and Stage 2 (`lot`, eight tests) wired against their real CLIs.

Stage modules `reel`, `vault`, `epic`, and `mech` remain stubs filled in
by D7-D8.

## Stage 0: prerequisite check

Before any test stage runs, gate verifies the environment is ready:

1. Every required backlot binary is on disk.
2. `~/.flick/providers` exists and is non-empty.
3. `~/.flick/models` contains aliases `fast`, `balanced`, and `strong`.
4. `lot setup --check` exits 0 (sandbox prerequisites granted).

A failure prints **all** problems found in the single check pass, names
the command that fixes each, and exits with code `2`. The summary
table is **not** printed in this case -- no stages ran.

## Build

```sh
cargo build              # builds gate alongside every other backlot binary
```

## Usage

```sh
gate                             # run all stages
gate --only flick                # run exactly one stage
gate --from reel                 # skip stages before reel
gate --verbose                   # write results.json (implies --keep-scratch; transcripts deferred)
gate --bin-dir ./target/release  # use release binaries
gate --timeout 120               # override per-stage wall-clock timeout
gate --output-dir ./gate-out     # redirect results.json (transcripts deferred)
gate --keep-scratch              # preserve the per-run scratch tree on success
```

`--only` and `--from` are mutually exclusive.

### Exit codes

| Code | Meaning |
|------|---------|
| `0`  | Every executed stage passed (soft-fails do not count) |
| `1`  | At least one stage produced a hard `Fail` |
| `2`  | Prerequisite check failed (binary discovery, scratch creation, Stage 0 `prereqs` check, or `--verbose` output-write failure) |

### Binary discovery

Resolution order:

1. `--bin-dir <path>` if provided -- used verbatim.
2. The directory containing gate's own executable (with the `cargo test`
   `deps/` layer stripped if present).
3. `<workspace>/target/debug` as a last-resort fallback.

A discovery failure lists **every** missing binary in one error rather
than failing one at a time, so a single `cargo build` covers the gap.

### Scratch directories

Per-run scratch trees live under `target/gate-scratch/run-YYYYMMDD-HHMMSS/`
with one subdirectory per stage. The location is project-local because
Windows AppContainer ancestor-traverse ACEs cannot be granted under
`%TEMP%` / `C:\Users` (see workspace `CLAUDE.md`). On success the run dir
is removed; on hard failure (or with `--keep-scratch` / `--verbose`) it
is preserved and its path is printed.

## Tests

```sh
cargo test -p gate
```

Gate's own unit tests are pure -- they do not require credentials, network,
or a real sandbox. The expensive E2E stages run only when `gate` is
invoked from the command line.
