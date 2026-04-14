# mech

Declarative YAML workflow engine for LLM-driven control and dataflow.

Mech lets you describe multi-step LLM workflows as YAML files — functions made
of prompt and call blocks, with CEL guards routing execution and `depends_on`
expressing data-flow parallelism. The mech runtime validates, plans, and
executes those workflows against a reel agent backend.

## Workspace

| Crate | Type | Description |
|-------|------|-------------|
| `mech` | library | Workflow loader, validator, runtime, CEL engine, cue integration |
| `mech-cli` | binary (`mech`) | CLI — `mech validate` and `mech run` |

## Relationship to siblings

| Project | Role |
|---------|------|
| flick | LLM primitive — single model call, tool declaration (not execution), JSON result |
| reel | Agent session runtime — tool loop, NuShell sandbox, built-in tools |
| lot | Process sandboxing — AppContainer (Windows), namespaces + seccomp (Linux), Seatbelt (macOS) |
| cue | Generic recursive task orchestration framework |
| epic | Recursive problem-solver orchestrator that drives reel agents |
| **mech** | YAML workflow engine — prompt/call block CDFG, CEL expressions, reel execution |

## Design principles

- **YAML-not-a-language.** Workflows are data, not programs. No custom grammar:
  functions, blocks, schemas, and transitions are all standard YAML structures.
- **CEL as the universal expression language.** Every guard (`when:`), template
  interpolation (`{{...}}`), and variable assignment (`set_context`,
  `set_workflow`) evaluates CEL — one language, one mental model.
- **Unified CDFG.** Prompt blocks and call blocks live in the same control- and
  data-flow graph. Edges are either imperative transitions (`goto:`) or
  explicit data dependencies (`depends_on:`). Both forms compose freely.
- **Agent-config cascade.** Agent configuration (`model`, `grant`, `tools`,
  `write_paths`, `timeout`) flows from workflow → function → block level, with
  each level able to override or extend the one above.
- **Conversation-transparent call blocks.** When a function calls another, the
  callee starts with an empty conversation; the caller's message history is
  unaffected. Nesting is clean and predictable.
- **Cue integration for retry and escalation.** Each workflow function is a
  `cue::TaskNode` leaf. The cue orchestrator drives retry budgets, model
  escalation, and fix loops without mech owning that logic.

## Requirements

Rust 1.85+ (edition 2024).

## Build

```sh
cargo build -p mech
cargo build -p mech-cli
```

## Quick start

### 1. Write a workflow YAML

```yaml
# greet.yaml
functions:
  greet:
    input:
      type: object
      required: [name]
      properties:
        name: { type: string }
    blocks:
      say_hello:
        prompt: "Say hello to {{input.name}} in one sentence."
        schema:
          type: object
          required: [greeting]
          properties:
            greeting: { type: string }
```

### 2. Validate

```sh
mech validate greet.yaml
```

Exits 0 on success; prints a list of validation errors and exits 1 on failure.

### 3. Run

```sh
mech run greet.yaml --input '{"name": "Alice"}'
```

> **Note:** The standalone `mech` CLI uses a stub agent that returns an error
> for any LLM call. To execute workflows against a real model, embed mech as a
> library and supply a [`reel`](../reel)-backed `AgentExecutor`.

## Concepts

### Functions

A workflow file contains one or more named functions, each with an `input`
JSON Schema, an optional `output` schema, and a map of blocks.

### Blocks

Two block types:

| Type | Key | What it does |
|------|-----|--------------|
| **prompt** | `prompt:` | Sends a CEL-interpolated string to the agent and collects structured output against `schema:` |
| **call** | `call:` | Invokes another function by name, with `input:` and `output:` field mappings |

### Transitions and guards

Prompt and call blocks can have a `transitions:` list. Each entry has an
optional `when:` CEL guard and a required `goto:` target block. Guards are
evaluated in order; the first match wins. An entry without `when:` is the
unconditional fallback.

```yaml
transitions:
  - when: 'output.category == "urgent"'
    goto: escalate
  - goto: standard_response
```

### Context variables

Two scopes hold mutable state:

| Variable | Lifetime | Access |
|----------|----------|--------|
| `context.*` | Per function invocation | `set_context:` in any block |
| `workflow.*` | Entire workflow run | `set_workflow:` in any block |

Variables must be declared with a type and initial value before use.

### Schemas

Every prompt block declares a `schema:` for its output. Schemas may be inline
JSON Schema objects or `$ref:#<name>` references to the workflow-level
`schemas:` map. The function's return schema can be declared as `output:`,
as a `$ref:#<name>`, or left as `infer` (mech derives it from terminal blocks).

Shared schemas may themselves contain nested `$ref:#<name>` references in their
properties, array items, or combinator members (`allOf`/`anyOf`/`oneOf`). These
are resolved recursively at registry build time and at prompt-block schema
lookup time, with cycle detection (returns `SchemaRefCircular`) and missing-ref
detection (returns `SchemaRefUnresolved`).

### Agent configuration

Each function and block can specify an `agent:` block overriding the model,
grant flags, custom tools, write paths, and timeout. Named agent configurations
live in `workflow.agents` and are reused via `agent: "$ref:#<name>"`. An
`extends:` key copies a named config and applies only the listed overrides.

### Execution modes

| Mode | Trigger | Behaviour |
|------|---------|-----------|
| **Imperative** | Block has `transitions:` | Blocks execute serially; guards select the next block |
| **Dataflow** | Block has `depends_on:` only | Blocks execute in topological order; independent blocks may run in parallel |

Both modes can coexist within the same function.

## Example workflows

See [`mech/examples/`](examples/) for ready-to-run workflow definitions:

| File | What it shows |
|------|---------------|
| [`imperative_routing.yaml`](examples/imperative_routing.yaml) | Transitions, CEL guards, conditional branching |
| [`dataflow_pipeline.yaml`](examples/dataflow_pipeline.yaml) | `depends_on`, parallel block execution, upstream output references |
| [`function_composition.yaml`](examples/function_composition.yaml) | Call blocks, `input`/`output` mappings, function composition |

## CLI reference

```
mech validate <file>
mech run <file> [--function <name>] --input <json>
```

| Command | Description |
|---------|-------------|
| `mech validate <file>` | Parse and validate a workflow YAML; print errors and exit 1 on failure |
| `mech run <file> --input <json>` | Run the first (or named) function with the given JSON input; print the result to stdout |

| Flag | Description |
|------|-------------|
| `--function <name>` | Function to run (default: first function in the file) |
| `--input <json>` | JSON object supplying the function's input (required for `run`) |

## Library usage

```rust
use mech::{WorkflowLoader, WorkflowRuntime, AgentExecutor};

// 1. Load and validate the workflow from disk.
let workflow = WorkflowLoader::new().load("greet.yaml")?;

// 2. Construct the runtime, supplying your AgentExecutor implementation.
//    In production this wraps a reel::Agent; in tests you can use a stub.
let runtime = WorkflowRuntime::new(&workflow, &my_agent_executor);

// 3. Run a function.
let output = runtime.run("greet", serde_json::json!({"name": "Alice"})).await?;
println!("{}", serde_json::to_string_pretty(&output)?);
```

`WorkflowLoader::load_str(yaml)` accepts a YAML string for in-memory loading
without touching the filesystem — useful for tests and embedded callers.

## Full specification

[`docs/MECH_SPEC.md`](../docs/MECH_SPEC.md) — complete language specification
covering the unified CDFG model, all 24+ load-time validation checks, CEL
namespace rules, schema inference algorithm, cue integration protocol, and a
fully worked example.

## License

MIT OR Apache-2.0
