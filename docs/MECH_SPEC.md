# Mech — Workflow Definition Spec

> **Status:** Spec in progress. Not ready for implementation.
>
> **Crate:** `mech` — a standalone crate, not part of `cue`. Cue provides generic task orchestration; mech provides the declarative workflow definition format and executor. Mech depends on cue (for `TaskNode` integration) and reel (for agent execution), but is a separate compilation unit.

## 1. Overview

A YAML-based workflow definition format for agent workflows expressed as typed, statically-validated graphs. Each workflow file declares one or more **functions** — callable units whose bodies are hybrid control-flow/data-flow graphs (CDFGs). Blocks within a function are LLM prompt calls with JSON Schema typed inputs and outputs, connected by control edges (CEL-guarded transitions) and data edges (`depends_on`).

Mech is not a custom language — there is no bespoke grammar or parser. The workflow format is defined by a YAML schema (§12) with CEL as the embedded expression language for guards, templates, and mappings.

### Motivation

Rust is the right language for the backlot runtime — type safety, tooling, performance. But Rust is too general-purpose for rapid iteration on task logic. Each new task type requires Rust code changes, recompilation, and the full development cycle. Meanwhile, dynamic languages (Python, JS) offer fast iteration but lack the rigid type systems and validation that LLM orchestration requires — models benefit from structural constraints, not flexibility.

Mech occupies the middle ground: **declarative structure with static typing, without requiring compilation.** Workflow authors define what each block does (prompt + schema), how blocks connect (transitions + dependencies), and what expressions govern routing (CEL) — all in YAML files that can be modified, reloaded, and tested without touching Rust.

Mech replaces the need to implement task types as Rust code. The cue orchestrator executes mech workflow functions the same way it executes native tasks — the function is the unit of work, the CDFG is the implementation.

### Relationship to Cue and Reel

Mech is a standalone crate with two key dependencies:

- **Cue** provides generic recursive task orchestration (`TaskNode`, `TaskStore`, `Orchestrator`). A mech workflow function can serve as a cue task's implementation (§11). The orchestrator drives decomposition, retry, escalation; mech drives the internal logic of each task.
- **Reel** provides the agent runtime. Each prompt block executes as a reel agent run — model selection, tool grants, sandbox, tool loop. Mech configures reel via the `agent` block (§5.5).

Mech does not live inside either crate. It is its own compilation unit that bridges cue's orchestration with reel's agent execution through a declarative YAML surface.

## 2. Design Goals

1. **Single unified graph model.** No mode selection. Control edges and data edges coexist freely in one graph. The executor infers behavior from edge types present on each block.
2. **Functions as the callable unit.** A workflow file defines named functions. Functions call other functions. Parallelism is expressed at the function-call level (fork/join), not as a graph-level mode.
3. **Static typing via JSON Schema.** Every block declares its output schema. Type mismatches between a block's output and a downstream block's template references are caught at load time, not runtime.
4. **CEL for all expressions.** Transition guards, template expressions, and any computed values use CEL. No embedded Python, no custom expression language, no eval.
5. **YAML surface syntax.** Human-readable, LLM-readable, tooling-friendly. No custom parser required for the outer structure.
6. **Declarative, not imperative.** Workflows describe structure and constraints. The executor decides scheduling, parallelism within dataflow regions, and retry mechanics.
7. **Embeddable in cue.** A mech workflow function maps to a cue `TaskNode` implementation. Mech does not replace cue's orchestration protocol — it provides a declarative way to define what a task does internally.

## 3. Core Concepts

- **Workflow file** — A YAML file declaring one or more functions.
- **Function** — A named callable unit. Its body is a CDFG (control-data flow graph) of blocks. Functions can call other functions.
- **Block** — A node in the graph. Prompt blocks invoke an LLM with a prompt template and validate the output against a JSON Schema. Call blocks invoke another function (with optional fork/join for parallelism).
- **Control edge** — A `transition` from one block to another, optionally guarded by a CEL expression (`when`). Evaluated in declaration order; first match wins. Supports cycles (self-loops, backward edges).
- **Data edge** — A `depends_on` declaration. The block cannot execute until all named dependencies have produced output. Acyclic by definition.
- **Activation rule** — A block with inbound control edges is *activated* when a transition targets it. A block with only data edges is activated implicitly when its dependencies are met. A block with both requires the transition to fire AND all dependencies to be satisfied. (Control gates activation; data gates readiness.)
- **Schema** — JSON Schema (inline YAML or `$ref` path) declaring the typed output of a block. Used for load-time validation of downstream template references.
- **Template expression** — `{{...}}` references interpolated into prompt text. The expression inside the braces is a CEL expression evaluated against the available namespaces (`input`, `output`, `context`, `workflow`, `blocks`). Simple paths like `{{input.text}}` and computed values like `{{context.score >= 0.8 ? "high" : "low"}}` both work. Scoping rules defined in §7.
- **Guard** — A CEL expression on a transition. Evaluated against the current block's output, function context, and workflow context.
- **Context** — Mutable typed variables declared with initial values. Two levels: **workflow context** (`workflow.*`, shared across all function invocations) and **function context** (`context.*`, scoped to a single invocation). Variables are pre-declared — blocks can only write to declared variables, and all variables always exist.
- **Agent configuration** — Runtime environment for prompt block execution. Configures the reel agent: model, grant flags (TOOLS/WRITE/NETWORK), custom tool names, writable paths, and timeout. Follows a three-level cascade (workflow → function → block) with replace semantics. Named configurations can be defined at the workflow level and referenced via `$ref:#name` or extended via `extends`.

## 4. Graph Model — Unified CDFG

No mode selection. A function's body is a **Control-Data Flow Graph (CDFG)**: `G = (V, E_control, E_data)`. Both edge types coexist freely on the same graph. The executor infers scheduling from the edges present.

### 4.1 Control Edges (Transitions)

Blocks connected via `transitions` with CEL guards. Evaluated in declaration order; first truthy guard wins. A transition with no `when` is an unconditional fallback. Cycles are permitted (self-loops, backward edges to earlier blocks).

Control edges determine **reachability** — whether a block will execute at all.

### 4.2 Data Edges (Dependencies)

Blocks declare `depends_on: [block_a, block_b]`. The block cannot begin until all named dependencies have produced output. Data edges are acyclic (enforced at load time).

Data edges determine **readiness** — when a reachable block may begin executing.

### 4.3 Activation Rule

How a block fires depends on which inbound edge types it has:

| Inbound control edges | Inbound data edges | Activation rule |
|---|---|---|
| None | None | Entry point. Fires at function start. |
| None | Yes | Dataflow node. Fires when all `depends_on` are satisfied. |
| Yes | None | CFG node. Fires when a transition targets it. |
| Yes | Yes | Hybrid. Transition must fire (activation), then all `depends_on` must be satisfied (readiness). |

**Execution model for dataflow regions:**

1. **Backward dependency walk.** Starting from terminal blocks (or blocks targeted by outbound control edges), walk `depends_on` edges backward to identify the reachable subgraph.
2. **Dead node elimination.** Blocks not reachable backward from any terminal or control-edge target are never executed.
3. **Topological sort.** Reachable blocks are sorted into execution levels by dependency depth.
4. **Level scheduling.** The executor advances level-by-level. Blocks within the same level (no mutual dependencies) execute sequentially today; within-level parallel execution is future work.
5. **Multiple sinks.** If multiple terminal blocks exist in a dataflow region, shared upstream blocks execute exactly once.

### 4.4 Function Calls

A **call block** invokes one or more functions. `call` accepts three forms:

1. **Single function** — a string naming one function. The block-level `input` maps to that function.
2. **Uniform list** — a list of strings. All functions share the block-level `input` (all must accept the same input fields).
3. **Per-call list** — a list of `{ fn, input }` objects. Each call carries its own input mapping, allowing heterogeneous function signatures.

Execution is **sequential by default**. The optional `parallel` property opts into concurrent execution and specifies the join strategy.

```yaml
# Single function — block-level input
lookup:
  call: sentiment_check
  input: { text: "{{input.text}}" }

# Uniform list — shared input, sequential
pipeline:
  call: [extract, validate, transform]
  input: { text: "{{input.text}}" }

# Per-call list — each call has its own input, parallel
analyze:
  call:
    - fn: sentiment_check
      input: { text: "{{input.text}}" }
    - fn: policy_lookup
      input: { query: "{{input.text}}", category: "{{context.category}}" }
    - fn: translation
      input: { text: "{{input.text}}", target_lang: "en" }
  parallel: all       # all | any | n_of_m
```

**Input mapping rules:**

- **Single function or uniform list:** The block-level `input` field is required and maps to all called functions.
- **Per-call list:** Each entry carries its own `input`. A block-level `input` is forbidden (ambiguous which takes precedence).
- Detection: if `call` is a list and the first element is an object (has `fn` key), the list is per-call. If the first element is a string, the list is uniform.

**Output mapping:** A call block may declare an optional `output` field — a map of field names to template/CEL expressions that construct the block's output from the called functions' results. Expressions can reference each called function's result by name (`<fn_name>.output.*`), plus `input` and `context` from the caller's scope.

```yaml
analyze:
  call:
    - fn: sentiment_check
      input: { text: "{{input.text}}" }
    - fn: policy_lookup
      input: { query: "{{input.text}}", category: "{{context.category}}" }
  parallel: all
  output:
    sentiment: "{{sentiment_check.output.score}}"
    policies: "{{policy_lookup.output.policies}}"
```

If `output` is omitted, the default applies: for a single function call, the block's output is the function's return value; for list calls, the output is the last function's return value (sequential) or a map of function names to outputs (parallel `all`).

**Sequential list execution:** Functions execute in list order. Each function's output is accessible by name via `{{blocks.<name>.output.*}}` in subsequent blocks. The call block's own `output` is determined by the `output` mapping if present, otherwise the last function's return value.

**Parallel execution:** Functions execute concurrently as independent CDFGs. Results are collected per the join strategy:

| Strategy | Behavior |
|---|---|
| `all` | Wait for every function to complete. |
| `any` | Resume when the first function completes. Others are cancelled. |
| `n_of_m` | Resume when `n` functions complete (requires `n:` field). Others are cancelled. |

**Cancellation:** When `any` or `n_of_m` triggers early completion, remaining in-flight functions receive a cancellation signal. A cancelled function's output is not available — template references to cancelled functions are a runtime error. Callers using `any` or `n_of_m` should only reference outputs conditionally or use the join result which identifies which functions completed.

**Result collection:** All completed function outputs are accessible via `{{blocks.<name>.output.*}}` regardless of execution mode. For `any`, only the winning function's output is populated. For `n_of_m`, outputs of the `n` completed functions are populated; the rest are absent. If an `output` mapping is declared, it is evaluated after result collection — the mapping expressions see all completed function outputs.

### 4.5 Function Definitions

A function declares its **input schema** (typed arguments), an optional **output schema** (typed return value), and zero or more **terminal blocks**.

```yaml
functions:
  sentiment_check:
    input:
      type: object
      required: [text]
      properties:
        text: { type: string }
    output:                # explicit output schema; omit to infer from terminals
      type: object
      required: [summary, label]
      properties:
        summary: { type: string }
        label: { type: string }
    terminals: [result]    # explicit; omit to auto-detect

    blocks:
      analyze:
        prompt: |
          Rate the sentiment of: {{input.text}}
        schema:
          type: object
          required: [score, label]
          properties:
            score: { type: number }
            label: { type: string, enum: [positive, neutral, negative] }
        transitions:
          - goto: result

      result:
        prompt: |
          Summarize: score={{blocks.analyze.output.score}}, label={{blocks.analyze.output.label}}
        schema:
          type: object
          required: [summary, label]
          properties:
            summary: { type: string }
            label: { type: string }
```

**Output schema** declares the function's return type — the schema that callers can validate against.

- **Explicit schema:** An inline JSON Schema object or `$ref` string. The loader validates that all terminal blocks produce output compatible with this schema.
- **`infer`:** The string literal `"infer"`. The loader derives the output schema from the terminal blocks (see inference rules below).
- **Omitted:** Defaults to `infer`.

**Output schema inference rules (Option A — keyed map):**

- **Single terminal (either mode):** output schema = that terminal's schema directly. Callers reference `{{callee.output.field}}`.
- **Multiple terminals, dataflow sinks:** output schema is a keyed-map object — `{type: object, required: [t1, t2, …], properties: {t1: schema1, t2: schema2, …}}`. The runtime collects all dataflow sink outputs into a JSON object keyed by block name; the inferred schema mirrors that shape. Callers reference `{{callee.output.terminal_name.field}}`.
- **Multiple terminals, CFG (imperative) paths:** all terminals must share the same output schema (structural equality after `$ref` resolution). If they do, that shared schema is used. If they differ, inference fails — declare an explicit `output:` schema and document which terminal the caller should expect. (CFG paths reach exactly one terminal at runtime, so only one schema applies per execution; requiring them to be identical lets `infer` work without a discriminator.)
- **No terminals detected:** load-time error — the author must declare an explicit output schema or fix the terminal detection.

**Terminal blocks** determine the function's return value:

- If `terminals` is specified: those blocks are terminal. Validated at load time (must exist, must have no outgoing transitions or data edges).
- If `terminals` is omitted: terminal blocks are inferred — any block with no outgoing control edges and no outgoing data edges.
- **Single terminal reached:** the function's output is that block's output.
- **Multiple terminals (CFG paths):** the function's output is the output of whichever terminal was reached during execution. (Schema: all terminals share the same schema per the inference rule above.)
- **Multiple terminals (dataflow sinks):** all terminal outputs are collected into a JSON object keyed by block name, e.g. `{"sink_a": {...}, "sink_b": {...}}`. (Schema: a keyed-map object with one property per sink, matching this runtime shape.)

### 4.6 Conversation Model

Each function invocation creates a new **conversation** — a system prompt (stored in a dedicated conversation slot, not as a message in the history) plus an ordered list of user/assistant/tool messages that is passed to the LLM on each block execution within that function. Conversation history follows **control edges only**. Data edges carry structured output, never conversation history.

**Core rules:**

1. **Function = conversation boundary.** A function invocation creates a fresh, empty conversation. When the function returns, its conversation is discarded. The caller sees only the function's structured output — analogous to a stack frame that is popped on return.
2. **Control edges carry history forward.** When a transition fires from block A to block B, block B's LLM call includes the full conversation accumulated along the control-flow path that reached it. Each prompt block appends a user message (the rendered prompt) and an assistant message (the LLM's structured response) to the conversation. **The append is atomic and conditional on schema validation passing** — if the LLM's output fails to validate against the block's `schema`, the conversation is left unchanged (this includes any tool call/result messages produced during the agent's internal loop — the entire `response.messages` list is discarded atomically on validation failure). This guarantees clean retry semantics: a self-loop or backward transition triggered after a validation failure sees the same history it saw before the failed attempt, with no bogus turn polluting subsequent retries.
3. **Data edges do not carry history.** A block activated by `depends_on` receives its dependencies' structured outputs via template variables (`{{blocks.<name>.output.*}}`), but does not inherit their conversation history. Dataflow blocks are single-turn by nature.
4. **Call blocks are conversation-transparent.** A `call` block invokes a sub-function, which starts with its own empty conversation. The caller's conversation is unchanged — the call block contributes only structured output, not conversation history. The sub-function's internal conversation is invisible to the caller.
5. **Parallel branches are conversation-isolated.** Parallel function calls (via `parallel: all|any|n_of_m`) each get independent conversations. No merge problem exists because there is no shared history to merge.

**Cycles and history accumulation:**

Self-loops and backward transitions accumulate conversation history. A block that transitions back to itself (retry pattern) sees its prior prompt+response pairs on each iteration. This is intentional — the LLM benefits from seeing its prior attempts. Workflow authors should use `LimitsConfig` (retry budgets) or CEL guards (e.g., `context.attempts < 3`) to bound cycles and prevent unbounded history growth.

**Implications for mixed CDFG graphs:**

In a function with both control edges and data edges, the conversation follows the control-flow spine. Dataflow blocks within a level execute sequentially today (within-level parallelism is future work, see §13); each is single-turn, receiving structured data from its dependencies but no conversational context. The single-turn rule is permanent: even when parallel execution lands, dataflow blocks will not share conversation history.

A hybrid block (inbound control edge + inbound data edges, per §4.3) inherits conversation from the control edge that activated it. Its data dependencies contribute structured output only.

**System prompts are layered: workflow default + function override.** The workflow file may declare a default `system` field. Each function may override it with its own `system` field. The resolved system prompt is rendered once at function entry and stored in a dedicated `Conversation.system` slot, separate from the message history. It is delivered to the agent through `AgentRequest.system` (not as the first element of the message list) so executors that prepend system to history themselves do not see it duplicated. System prompts support template variables (`{{input.*}}`, `{{context.*}}`), allowing the caller to parameterize persona.

```yaml
workflow:
  system: "You are a support agent. Be helpful and professional."

functions:
  triage:
    # inherits workflow system prompt
    blocks: { ... }

  billing:
    system: "You are a billing specialist for {{input.company}}."
    input:
      type: object
      required: [company, issue]
      properties:
        company: { type: string }
        issue: { type: string }
    blocks: { ... }
```

If neither the function nor the workflow declares a `system` field, the conversation's system slot is empty and `AgentRequest.system` is `None`. Per-block system prompt variation is not supported — per-block instructions belong in the block's `prompt` template. If a block needs a fundamentally different persona, extract it to a separate function.

**History compaction.** Long-running functions (especially those with cycles) accumulate conversation history that may exceed the model's context window. Mech provides a token-budget compaction mechanism, modeled after Pi Agent's approach.

> **Implementation status:** Not implemented (placeholder). The hook fires at the configured token threshold and increments a counter, but messages are not summarized. Workflows that configure `compaction` receive a `tracing::warn!` at load time (a `LoadWarning::CompactionPlaceholder` is emitted; the same advisories are also exposed programmatically via the `pub` test seam `mech::loader::collect_load_warnings`). Real LLM-based summarization, custom-function dispatch, and accurate token-budget triggers are future work.

When a function's conversation exceeds a token threshold, compaction fires: older messages are summarized into a synthetic message that replaces them at the head of the conversation. Recent messages (within `keep_recent_tokens`) are preserved verbatim. The summary is generated by an LLM call using a structured format (goal, progress, decisions, key context), and the previous summary is fed as iterative context so information degrades gracefully rather than being hard-truncated.

Configuration is per-function via an optional `compaction` field:

```yaml
functions:
  long_running_task:
    system: "You are a research assistant."
    compaction:
      keep_recent_tokens: 20000    # preserve this many tokens of recent history
      reserve_tokens: 16384        # trigger when: used > context_window - reserve
    blocks: { ... }
```

If `compaction` is omitted, the executor uses workflow-level defaults. If no defaults exist, compaction is disabled (the full conversation is sent on every call; the workflow author is responsible for bounding cycles).

**Custom compaction functions.** The `compaction` field accepts an optional `fn` property naming a mech workflow function (or registered Rust handler) that replaces the built-in summarizer. The custom function receives the messages to summarize as input and returns a summary string. This allows domain-specific compaction logic (e.g., preserving specific structured data, using a cheaper model for summarization, or applying non-LLM compression).

```yaml
compaction:
  keep_recent_tokens: 20000
  reserve_tokens: 16384
  fn: custom_summarizer         # a mech workflow function or registered handler
```

**Agent configuration is per-block.** Each prompt block executes as a reel agent run — not a raw LLM call. The `agent` block configures the runtime environment: model, grant flags, custom tools, writable paths, and timeout. Agent configuration follows a three-level cascade (workflow → function → block) with replace semantics (§5.5). Different blocks within the same conversation can use different agent configurations — the executor reconfigures the reel agent between turns while preserving the shared conversation history. Conversation history is agent-agnostic; agent configuration is per-block execution.

```yaml
blocks:
  draft:
    agent:
      model: haiku
      grant: [tools]
    prompt: "Write a first draft of {{input.topic}}"
    schema: { ... }
    transitions:
      - goto: critique

  critique:
    agent:
      model: opus
      grant: [write]
      write_paths: [drafts/]
    prompt: "Critique the draft and rewrite weak sections."
    schema: { ... }
    transitions:
      - when: 'output.quality < 0.8'
        goto: draft
      - goto: done
```

**Design rationale:**

The stack-frame analogy: a called function cannot see the caller's conversation, and the caller cannot see the callee's internal conversation. Communication is through explicit typed inputs and outputs. This gives workflow authors a single, predictable mechanism for conversation scoping — function extraction — rather than per-block or per-edge configuration.

### 4.7 Theoretical Basis

The CDFG model is well-established in compiler theory (Ferrante et al., 1987 — Program Dependence Graph; Click & Paleczny — Sea of Nodes). The "control dominates, data constrains" resolution used here matches the PDG model: control-dependence edges determine whether a node is reached, data-dependence edges constrain ordering within reachable regions.

## 5. Block Specification

A block is a named node in a function's CDFG. Two block types exist: **prompt blocks** (invoke an LLM) and **call blocks** (invoke another function). The type is determined by which required field is present — `prompt` or `call`. They are mutually exclusive.

### 5.1 Prompt Block

Invokes an LLM with a rendered prompt template. Validates the response against a JSON Schema. Appends the prompt/response pair to the function's conversation history (if on a control-flow path).

| Field | Type | Required | Description |
|---|---|---|---|
| `prompt` | string | Yes | Template string with `{{...}}` CEL expressions (§7). Becomes the user message in the conversation. |
| `schema` | object \| string | Yes | JSON Schema for the LLM's structured output. Inline YAML object or `$ref` string path (§8). |
| `agent` | object \| string | No | Agent configuration for this block. Inline object, `$ref:#name`, or `$ref:path`. If omitted, inherits from function or workflow default. See §5.5. |
| `transitions` | list | No | Outbound control edges. Each entry has `goto` (required) and `when` (optional CEL guard). See §6. |
| `depends_on` | list of strings | No | Block names whose outputs must be available before this block executes. Acyclic (enforced at load time). |
| `set_context` | object | No | CEL expressions writing to function context variables. Keys must be declared in the function's `context`. See §9. |
| `set_workflow` | object | No | CEL expressions writing to workflow context variables. Keys must be declared in `workflow.context`. See §9. |

**Minimal prompt block:**

```yaml
classify:
  prompt: "Classify this text: {{input.text}}"
  schema:
    type: object
    required: [category]
    properties:
      category: { type: string, enum: [billing, technical, general] }
```

**Full prompt block:**

```yaml
analyze:
  agent:
    model: sonnet
    grant: [tools]
  prompt: |
    Given the classification {{blocks.classify.output.category}},
    analyze the customer's issue in detail.
    Previous context: {{context.summary}}
  schema:
    type: object
    required: [analysis, severity]
    properties:
      analysis: { type: string }
      severity: { type: integer, minimum: 1, maximum: 5 }
  depends_on: [classify]
  set_context:
    last_severity: "output.severity"
    attempt_count: "context.attempt_count + 1"
  transitions:
    - when: 'output.severity >= 4'
      goto: escalate
    - goto: respond
```

### 5.2 Call Block

Invokes one or more named functions. The called function(s) execute with their own conversation and return structured output. Call blocks are transparent to the parent conversation — they produce output but add no messages to the caller's history.

| Field | Type | Required | Description |
|---|---|---|---|
| `call` | string \| list of strings \| list of call entries | Yes | Function name(s) to invoke. Single string, uniform list (shared input), or per-call list (`{ fn, input }` objects). See §4.4 for the three forms. |
| `input` | object | Conditional | Input mapping. Required for single-function and uniform-list calls. Forbidden for per-call list calls (each entry carries its own `input`). Keys are the called function's input field names, values are CEL expressions or template strings resolved in the caller's scope. |
| `output` | object | No | Output mapping. Keys are output field names, values are template/CEL expressions evaluated after the call completes. Expressions can reference called function results (`<fn_name>.output.*`), `input`, and `context`. If omitted, defaults to the last function's return value. See §4.4. |
| `parallel` | string | No | Join strategy for list calls: `all`, `any`, `n_of_m`. If omitted, list calls execute sequentially. Ignored for single-function calls. |
| `n` | integer | No | Required when `parallel: n_of_m`. Number of completions needed before resuming. |
| `transitions` | list | No | Outbound control edges (same as prompt blocks). |
| `depends_on` | list of strings | No | Block names whose outputs must be available before this block executes (same as prompt blocks). |
| `set_context` | object | No | CEL expressions writing to function context variables (same as prompt blocks). |
| `set_workflow` | object | No | CEL expressions writing to workflow context variables (same as prompt blocks). |

**Call entry fields** (per-call list only):

| Field | Type | Required | Description |
|---|---|---|---|
| `fn` | string | Yes | Function name to invoke. |
| `input` | object | Yes | Input mapping for this specific call. Same semantics as the block-level `input`. |

**Call block output:** If an `output` mapping is declared, the block's output is the object constructed from it. Otherwise: for a single function call, the block's output is the function's return value; for sequential list calls, the output is the last function's return value (all are accessible via `{{blocks.<fn_name>.output.*}}`); for parallel calls, see §4.4 result collection rules.

**Minimal call block:**

```yaml
lookup:
  call: sentiment_check
  input: { text: "{{input.text}}" }
```

**Uniform list call block:**

```yaml
pipeline:
  call: [extract, validate, transform]
  input: { text: "{{input.text}}" }
  transitions:
    - goto: next
```

**Per-call list block (heterogeneous functions):**

```yaml
analyze:
  call:
    - fn: sentiment_check
      input: { text: "{{input.text}}" }
    - fn: policy_lookup
      input: { query: "{{input.text}}", category: "{{context.category}}" }
    - fn: translation
      input: { text: "{{input.text}}", target_lang: "en" }
  parallel: all
  transitions:
    - goto: synthesize
```

**Per-call list with output mapping:**

```yaml
analyze:
  call:
    - fn: sentiment_check
      input: { text: "{{input.text}}" }
    - fn: policy_lookup
      input: { query: "{{input.text}}", category: "{{context.category}}" }
  parallel: all
  output:
    sentiment: "{{sentiment_check.output.score}}"
    policies: "{{policy_lookup.output.policies}}"
  transitions:
    - goto: synthesize
```

### 5.3 Field Validity Rules

| Field | Prompt block | Call block |
|---|---|---|
| `prompt` | Required | Forbidden |
| `schema` | Required | Forbidden |
| `agent` | Optional | Forbidden |
| `call` | Forbidden | Required |
| `input` | Forbidden | Conditional (required for single/uniform, forbidden for per-call list) |
| `output` | Forbidden | Optional |
| `parallel` | Forbidden | Optional |
| `n` | Forbidden | Optional (requires `parallel: n_of_m`) |
| `transitions` | Optional | Optional |
| `depends_on` | Optional | Optional |
| `set_context` | Optional | Optional |
| `set_workflow` | Optional | Optional |

**Load-time enforcement:** A block with both `prompt` and `call` is an error. A block with neither is an error. A block with `schema` or `agent` but no `prompt` is an error. A block with `output` but no `call` is an error. A block with `parallel` or `n` but no `call` is an error. A block with `n` but `parallel` not set to `n_of_m` is an error. A per-call list block with a block-level `input` is an error. A single-function or uniform-list block without a block-level `input` is an error. A per-call entry missing `fn` or `input` is an error.

### 5.4 Block Identity and Naming

Block names are the YAML keys under the `blocks:` map. Names must be valid identifiers: `[a-z][a-z0-9_]*` (lowercase, underscore-separated). Names must be unique within a function. Reserved names: `input`, `context`, `workflow`, `block`, `blocks`, `meta`, plus the synthetic `output` (these conflict with CEL namespace variables).

### 5.5 Agent Configuration

Each prompt block executes as a reel agent run. The `agent` block configures the runtime environment for that execution. Agent configuration follows a three-level cascade with **replace semantics** — each level fully replaces the level above, with no field-level merging.

#### 5.5.1 Agent Configuration Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `model` | string | No | Model name. Resolves via flick's `ModelRegistry`. |
| `grant` | list of strings | No | Reel `ToolGrant` flags: `tools`, `write`, `network`. `write` and `network` imply `tools` (auto-normalized). Default: no grants (structured-output-only, no tool loop). |
| `tools` | list of strings | No | Custom tool names to enable. Must be registered with the executor at runtime. |
| `write_paths` | list of strings | No | Fine-grained writable paths (relative to project root). Only meaningful when `grant` includes `write`. |
| `timeout` | string | No | Agent run timeout (e.g., `"30s"`, `"5m"`). If omitted, uses executor default. |
| `extends` | string | No | Name of a workflow-level named agent config to use as a base. Specified fields override the base; unspecified fields inherit from it. Mutually exclusive with `$ref` string form. |

#### 5.5.2 Three-Level Cascade

Resolution follows a three-level cascade:

1. **Block-level** `agent` field (highest priority)
2. **Function-level** `agent` field (declared alongside `input`, `system`, etc.)
3. **Workflow-level** `agent` field (declared at the top level)

If no agent config is specified at any level, the executor uses its runtime defaults (model, no grants, no custom tools).

**Semantics: replace, not merge.** When a lower level declares `agent`, it **completely replaces** the inherited config. A function-level agent config replaces the workflow default entirely; a block-level agent config replaces the function-level config entirely. There is no field-level merge — if a block specifies `agent` with only `model`, the block has only a model (no grants, no tools, no write_paths).

To override specific fields while inheriting the rest, use `extends` (§5.5.4).

#### 5.5.3 Named Agent Configurations

Workflow-level `agents` map defines reusable, named configurations — parallel to `schemas`:

```yaml
workflow:
  agents:
    reader:
      model: haiku
      grant: [tools]
    writer:
      model: sonnet
      grant: [write]
      write_paths: [src/]
    researcher:
      model: sonnet
      grant: [tools, network]
      tools: [web_search, fetch_url]

  agent: "$ref:#reader"          # workflow default references a named config
```

Named configs are base definitions only. They are defined once and referenced by name via `$ref:#name` (inline string form) or `extends` (inside an inline agent block at function or block level). External files are also supported: `$ref:agents/reader.yaml`.

Named configs may **not** use `extends` themselves — `extends` is only permitted on inline agent configs (function-level or block-level). A named agent entry in `workflow.agents` that contains an `extends` field is a load-time error (see §12.1).

#### 5.5.4 Reference and Extension

Three forms at any level (workflow, function, block):

**Direct reference** — use a named config as-is:

```yaml
agent: "$ref:#reader"
```

**Extend** — start from a named config, override specific fields:

```yaml
agent:
  extends: reader
  model: opus              # override model; grant inherited from 'reader'
```

`extends` starts from the named config and applies specified fields as overrides. This is the mechanism for "inherit with tweaks" under replace semantics. Only fields explicitly present in the extending block override the base — unspecified fields retain the base's values.

**Fully inline** — no inheritance:

```yaml
agent:
  model: haiku
  grant: [tools]
```

**External file reference:**

```yaml
agent: "$ref:agents/reader.yaml"
```

External files are loaded and inlined at load time, like schema `$ref`. The path is relative to the workflow file's directory.

**Detection:** The deserializer distinguishes forms by type: `string` starting with `$ref:` → reference (named or file); `object` → inline config (check for `extends` field to resolve base).

#### 5.5.5 Grant Semantics

Grant flags map directly to reel's `ToolGrant` bitflags:

| Grant | Reel effect | Tools enabled |
|---|---|---|
| `tools` | Read-only codebase access + NuShell sandbox | Read, Glob, Grep, NuShell |
| `write` | Implies `tools`. Adds write access to `write_paths` (or project root if unspecified). | adds Write, Edit |
| `network` | Implies `tools`. Enables network access in the sandbox. | (no additional tools) |

**Auto-normalization rules:**

- `write` and `network` imply `tools` (same as reel's `ToolGrant::normalize()`).
- Specifying `tools` (custom tool names) implies `grant: [tools]` — custom tools require the tool loop.

A prompt block with no `grant`, no `tools`, and no `agent` block runs in **structured-output-only mode** — a single LLM call with no tool loop. This is the default and matches the original spec behavior where prompt blocks were raw LLM calls.

Adding any grant (or any custom `tools`) activates reel's **tool-loop mode** — the agent can use tools across multiple rounds before producing its final structured output.

#### 5.5.6 Tool-Loop Conversation Interaction

When a block runs with grants (tool-loop mode), the reel agent executes an internal multi-turn conversation with tool calls. This internal conversation is **invisible to the function's conversation** (§4.6). From the function's perspective, the block contributes exactly one user/assistant exchange:

1. The rendered `prompt` becomes the **user message** in the function's conversation.
2. The agent's **final structured output** (validated against `schema`) becomes the **assistant message**.
3. The agent's internal tool-use turns (tool calls, tool results, intermediate reasoning) are discarded from the function's conversation.

This preserves the conversation model's invariant: each prompt block appends exactly one user message and one assistant message. Downstream blocks that inherit conversation history (via control edges) see only the prompt and final output, not the agent's internal tool interactions.

#### 5.5.7 System Prompt Interaction

`system` remains a separate field at workflow and function level (not inside `agent`). System prompts are a conversational concern — they use template expressions, participate in the conversation model (§4.6), and layer via workflow/function override. Agent configuration is a runtime environment concern — model, permissions, tools.

Both cascades are independent. A function can override `system` without touching `agent`, or override `agent` without touching `system`.

## 6. Transitions & Guards

Transitions are outbound control edges from a block. They determine which block executes next along the control-flow path. Each transition entry has a target (`goto`) and an optional guard (`when`).

### 6.1 Transition Entry

```yaml
transitions:
  - when: 'output.category == "billing"'    # CEL guard
    goto: billing_handler                     # target block name
  - when: 'output.category == "technical"'
    goto: tech_handler
  - goto: unknown_handler                     # unconditional fallback
```

| Field | Type | Required | Description |
|---|---|---|---|
| `goto` | string | Yes | Target block name. Must exist within the same function. |
| `when` | string | No | CEL expression. If omitted, the transition is unconditional. |

### 6.2 Evaluation Order

Transitions are evaluated **top-to-bottom, first match wins**. This is identical to an `if/else if/else` chain.

1. For each transition in declaration order:
   - If `when` is absent → match (unconditional).
   - If `when` is present → evaluate the CEL expression. If truthy → match.
2. First matching transition fires. The target block is activated.
3. If no transition matches → the block is terminal for this execution path. No error — the block simply has no successor.

**Unconditional fallback:** A transition with no `when` field matches unconditionally. It should appear last — any transitions after it are unreachable (flagged as a load-time warning).

### 6.3 CEL Expression Language

[CEL (Common Expression Language)](https://cel.dev/) is the single expression language used throughout mech — in `{{...}}` template expressions, `when` guards, `set_context`, call block `input` mappings, and call block `output` mappings. CEL is sandboxed, side-effect-free, and evaluates in constant time (no loops, no I/O).

**Available variables** depend on evaluation context:

| Variable | Template (`prompt`, `system`) | Template (`input`/`output` mapping) | `set_context` / `set_workflow` / `transitions` |
|---|---|---|---|
| `input` | Yes | Yes | Yes |
| `context` | Yes | Yes | Yes |
| `workflow` | Yes | Yes | Yes |
| `blocks.*` | Yes (executed predecessors) | Yes (executed predecessors / call results) | No (use `context`/`workflow`) |
| `output` | No (not yet produced) | No | Yes |

**Guard scope restriction:** `blocks.*` is not available in `when` guards, `set_context`, or `set_workflow`. If a guard needs upstream data, the workflow author should pipe it through `set_context` on an earlier block and reference `context.*` in the guard.

**CEL type mapping from JSON Schema:**

| JSON Schema type | CEL type |
|---|---|
| `string` | `string` |
| `number` | `double` |
| `integer` | `int` |
| `boolean` | `bool` |
| `array` | `list` |
| `object` | `map` |
| `null` | `null_type` |

**Common patterns:**

```yaml
# Equality
- when: 'output.status == "approved"'

# Numeric comparison
- when: 'output.confidence >= 0.9'

# Boolean field
- when: 'output.needs_review'

# String containment
- when: '"error" in output.tags'

# Existence check (optional fields)
- when: 'has(output.retry_reason)'

# Compound logic
- when: 'output.score < 0.5 && context.attempts < 3'

# List length
- when: 'size(output.items) > 0'
```

### 6.4 Self-Loops and Backward Edges

Transitions may target the current block (self-loop) or any earlier block in the control-flow path (backward edge). These create cycles in the control graph, which is permitted — cycles are only forbidden in data edges (`depends_on`).

**Self-loop (retry pattern):**

```yaml
draft:
  prompt: "Write a draft for {{input.topic}}"
  schema:
    type: object
    required: [text, quality_score]
    properties:
      text: { type: string }
      quality_score: { type: number }
  set_context:
    draft_attempts: "context.draft_attempts + 1"
  transitions:
    - when: 'output.quality_score >= 0.8'
      goto: finalize
    - when: 'context.draft_attempts < 5'
      goto: draft          # self-loop: retry with conversation history
    - goto: finalize       # budget exhausted: proceed anyway
```

Self-loops accumulate conversation history (§4.6) — each iteration sees prior prompt/response pairs. This is intentional for iterative refinement. Workflow authors must bound cycles via guards (e.g., `context.attempts < N`) or rely on the executor's compaction mechanism (§4.6) to manage history growth.

**Backward edge (return to earlier block):**

```yaml
transitions:
  - when: 'output.needs_reclassification'
    goto: classify         # backward edge to earlier block
  - goto: done
```

Backward edges reset the control-flow path to the target block. Conversation history accumulated since the target block's last execution is preserved (the conversation grows, it does not rewind).

### 6.5 No-Match Behavior

If a block has a `transitions` list but no transition matches (all guards are false, no unconditional fallback):

- The block becomes a **de facto terminal** for this execution path.
- The function produces the block's output as its return value.
- No error is raised — this is a valid (if unusual) way to end execution.

Load-time validation emits a **warning** (not error) for transition lists that lack an unconditional fallback, since the author likely intended one.

### 6.6 Transition Validation (Load-Time)

| Check | Severity |
|---|---|
| `goto` target does not exist in the function | Error |
| `when` expression fails CEL compilation | Error |
| Unconditional transition is not the last entry | Warning (unreachable transitions after it) |
| Transition list is empty (`transitions: []`) | Warning (equivalent to omitting `transitions`) |
| Guard references a variable not in scope (`output`, `input`, `context`) | Error |

## 7. Template Expressions & Scoping

Template expressions are `{{...}}` references interpolated into prompt text, `input` and `output` mappings on call blocks, and `system` prompt strings. The expression inside the braces is a **CEL expression** evaluated against the available namespaces. This is the same CEL used in `when` guards and `set_context` — one expression language everywhere.

Simple path references (`{{input.text}}`) and computed expressions (`{{size(blocks.extract.output.items)}}`, `{{context.attempts + 1}}`) are both valid.

### 7.1 Namespaces

| Namespace | Description |
|---|---|
| `input` | The function's input arguments (immutable for the function's lifetime). |
| `output` | The current block's own output. Only available in `set_context`, `set_workflow`, and `transitions` (the block must have already produced output). |
| `context` | The function's declared context variables (§9). Scoped to the current function invocation. |
| `workflow` | The workflow's declared context variables (§9). Shared across all function invocations. |
| `blocks` | Named block outputs. `blocks.<name>.output` accesses a block's structured output. The referenced block must have produced output (enforced by data edges or control-flow ordering). |

All five namespaces are CEL variables. See §6.3 for which variables are available in each evaluation context.

### 7.2 Resolution Rules

Template expressions are resolved **at render time** — just before the block executes (for `prompt`, `system`, and call block `input` fields) or just after (for `set_context`, `set_workflow`, `output` mappings, and `transitions`).

**Resolution order for a prompt block:**

1. Evaluate `{{...}}` CEL expressions in the `prompt` template. Available: `input`, `context`, `workflow`, `blocks.*`.
2. Send the rendered prompt to the LLM. Receive structured output.
3. Validate output against `schema`.
4. Evaluate `set_context` and `set_workflow` CEL expressions. Available: `output`, `input`, `context`, `workflow`.
5. Evaluate `transitions` guard CEL expressions. Available: `output`, `input`, `context`, `workflow`.

**Resolution order for a call block:**

1. Evaluate `{{...}}` CEL expressions in `input` field values (or per-call `input` entries). Available: `input`, `context`, `workflow`, `blocks.*`.
2. Invoke the called function(s) with resolved input.
3. Collect raw function output(s).
4. If `output` mapping is declared: evaluate its CEL expressions against the raw function outputs, `input`, `context`, and `workflow`. The result becomes the call block's output. Otherwise: apply the default (§4.4).
5. Evaluate `set_context`, `set_workflow`, and `transitions` against the call block's output.

### 7.3 Availability by Block Position

Which namespaces are available depends on how the block was activated:

| Block activation | `input` | `context` | `workflow` | `blocks.*` | `output` |
|---|---|---|---|---|---|
| Entry point (no inbound edges) | Yes | Yes | Yes | No (no predecessors) | After execution only |
| Control-flow target (via transition) | Yes | Yes | Yes | Only blocks on the control path that have executed | After execution only |
| Dataflow node (via `depends_on`) | Yes | Yes | Yes | Only declared dependencies | After execution only |
| Hybrid (control + data) | Yes | Yes | Yes | Dependencies + control-path predecessors | After execution only |

**Key constraint:** `blocks.<name>.output` references must be statically resolvable — the named block must be guaranteed to have executed before the referencing block. This is enforced at load time:

- A block referencing `blocks.foo.output` (in any CEL expression) must have `foo` in its `depends_on` list, OR `foo` must **dominate** the block in the control-flow graph (every control-flow path to the block passes through `foo`).
- If neither condition is met: load-time error.

### 7.4 CEL in Templates — Examples

**Simple path access** (identical to the old dotted-path syntax):

```yaml
prompt: |
  The user's name is {{input.user.name}}.
  Category: {{blocks.classify.output.category}}
```

**Nested field and array access:**

```yaml
prompt: |
  Top preference: {{blocks.preferences.output.items[0].label}}.
```

Array indexing uses bracket notation: `items[0]`, `items[1]`. Out-of-bounds access is a runtime error.

**Conditional expressions:**

```yaml
prompt: |
  Status: {{input.score >= 0.8 ? "high confidence" : "needs review"}}
  Attempt {{context.attempts}} of 3.
```

CEL's ternary operator and all standard functions work directly in templates. Since context variables are pre-declared (§9), they always exist — no `has()` checks needed.

**Computed values:**

```yaml
prompt: |
  Found {{size(blocks.extract.output.items)}} items.
  Average score: {{blocks.scores.output.total / blocks.scores.output.count}}
```

### 7.5 Template Syntax Details

- **Delimiters:** `{{` and `}}`. Literal braces in prompt text must be escaped as `{{"{"}}` and `{{"}"}}` (CEL string expression).
- **Whitespace:** `{{ input.text }}` is equivalent to `{{input.text}}` — leading/trailing whitespace inside delimiters is trimmed.
- **Expression language:** CEL (§6.3). The full CEL feature set is available: field access, arithmetic, comparisons, ternary (`? :`), `has()`, `size()`, string functions, list/map operations. No loops, no I/O, no side effects.
- **Rendering:** CEL expression results are serialized to their JSON string representation when interpolated into prompt text. Objects and arrays are rendered as compact JSON. Strings are rendered without quotes. Numbers and booleans render as their literal representation.
- **Evaluation error:** A CEL expression that fails (undefined variable, type mismatch, out-of-bounds access) is a **runtime error**. The block does not execute. The executor reports the expression, the error, and the block name.

## 8. Schema Handling

Every prompt block declares a `schema` — a JSON Schema that defines the expected structure of the LLM's output. Schemas serve two purposes: they constrain the LLM's response (via flick's structured output) and they enable load-time type checking of downstream template references.

### 8.1 Inline vs. External Schemas

The `schema` field accepts two forms:

**Inline YAML** (preferred for simple schemas):

```yaml
schema:
  type: object
  required: [category, confidence]
  properties:
    category: { type: string, enum: [billing, technical, general] }
    confidence: { type: number, minimum: 0, maximum: 1 }
```

**External `$ref`** (for large or shared schemas):

```yaml
schema: "$ref:schemas/resolution.json"
```

The `$ref:` prefix signals an external file path. The path is relative to the workflow file's directory. The referenced file must contain a valid JSON Schema document (JSON or YAML format). The schema is loaded and inlined at load time — no runtime file access.

**Detection:** The deserializer distinguishes the two forms by type: `string` starting with `$ref:` → external file path; `object` → inline schema.

### 8.2 Schema Requirements

All prompt block schemas must satisfy:

1. **Root type must be `object`.** The LLM's structured output is always a JSON object. Schemas with root type `array`, `string`, etc. are a load-time error.
2. **`required` array must be present** with at least one field. An empty-object schema (`type: object` with no properties) is a load-time error — it provides no structural constraint.
3. **JSON Schema draft 2020-12** is the supported dialect. Features beyond this draft are unsupported.

### 8.3 Load-Time Validation

At workflow load time, the loader performs these schema checks:

| Check | Description |
|---|---|
| Schema parse | Inline YAML parses to valid JSON Schema; external `$ref` file exists and parses. |
| Root type | Schema root `type` is `object`. |
| Required fields | `required` is non-empty. |
| Template type checking | For each `{{blocks.<name>.output.field}}` reference in downstream blocks, verify that the referenced block's schema declares the field and its type is compatible with the usage context. |
| Circular `$ref` | External schemas that reference each other are a load-time error. |

**Template type checking** is best-effort static analysis. The loader traces field access paths in CEL expressions (e.g., `blocks.foo.output.field.subfield`) through the schema's `properties` tree and verifies the field exists. Type mismatches (e.g., referencing `output.count` as a string when the schema declares it as `integer`) produce warnings, not errors — the LLM may return compatible values that don't match the JSON Schema type precisely. Computed CEL expressions (ternary, function calls) are not type-checked beyond verifying that referenced variables exist.

### 8.4 Runtime Validation

After each LLM call, the executor validates the response against the block's schema:

1. **Parse** the LLM's response as JSON.
2. **Validate** against the JSON Schema using the `jsonschema` crate.
3. **On success:** store the validated output, proceed to `set_context` and `transitions`.
4. **On failure:** the block fails. The executor reports the validation error (which fields failed, expected vs. actual types). The failure propagates per §10 runtime error handling.

**No coercion.** The executor does not attempt to fix or coerce malformed output. If the LLM returns `"42"` (string) for an `integer` field, validation fails. The structured output mechanism in flick is responsible for guiding the LLM to produce schema-compliant output — runtime validation is the safety net.

### 8.5 Schema Composition

Schemas can use JSON Schema composition keywords (`allOf`, `anyOf`, `oneOf`) for complex types. `$ref` within an inline schema references a definition within the same schema document (JSON Pointer), not an external file — external files use the `$ref:` prefix at the `schema` field level.

```yaml
schema:
  type: object
  required: [result]
  properties:
    result:
      oneOf:
        - type: object
          required: [answer]
          properties:
            answer: { type: string }
        - type: object
          required: [error]
          properties:
            error: { type: string }
```

For schemas shared across multiple blocks within the same workflow file, define them in the workflow-level `schemas` map and reference by name:

```yaml
workflow:
  schemas:
    resolution:
      type: object
      required: [resolved, notes]
      properties:
        resolved: { type: boolean }
        notes: { type: string }

functions:
  resolve_billing:
    blocks:
      attempt:
        prompt: "Resolve billing issue: {{input.issue}}"
        schema: "$ref:#resolution"
```

`$ref:#name` references the workflow-level `schemas` map. `$ref:path/file.json` references an external file. Plain `$ref` within a JSON Schema object uses standard JSON Schema `$ref` semantics (JSON Pointer).

## 9. Context & State

Context provides mutable state that persists across block executions. Two context levels exist: **workflow context** (shared across all function invocations) and **function context** (scoped to a single function invocation).

All context variables must be **declared** with a type and initial value. Blocks can only write to pre-declared variables. Because every variable is initialized at declaration, variables always exist — CEL expressions never need `has()` checks on context variables.

### 9.1 Declarations

**Workflow context** is declared in the `workflow.context` field:

```yaml
workflow:
  context:
    total_calls: { type: integer, initial: 0 }
    all_categories: { type: array, initial: [] }
```

**Function context** is declared in the `function.context` field:

```yaml
functions:
  support_triage:
    context:
      attempts: { type: integer, initial: 0 }
      best_score: { type: number, initial: 0.0 }
      all_results: { type: array, initial: [] }
    blocks: { ... }
```

Each declaration is a map entry: key is the variable name, value is an object with:

| Field | Type | Required | Description |
|---|---|---|---|
| `type` | string | Yes | JSON Schema type: `string`, `number`, `integer`, `boolean`, `array`, `object`. |
| `initial` | any | Yes | Initial value. Must be a literal compatible with the declared type. |

Variable names must be valid identifiers: `[a-z][a-z0-9_]*`. Workflow and function contexts occupy separate namespaces (`workflow.*` vs. `context.*`), so name collisions across levels are permitted but discouraged.

### 9.2 Two-Level Scoping

| Level | CEL namespace | Lifetime | Visibility |
|---|---|---|---|
| **Workflow** | `workflow.*` | Entire workflow execution | All functions — readable and writable |
| **Function** | `context.*` | Single function invocation | Only the declaring function's blocks |

**Workflow context** is created once when the workflow starts executing, initialized from declarations, and lives until the workflow completes. All functions — including nested calls — can read and write workflow context. This is the mechanism for cross-function state.

**Function context** is created when a function is invoked, initialized from declarations, and destroyed when the function returns. A called function cannot see the caller's function context — only its own declarations. This mirrors the conversation model (§4.6) — function = stack frame.

### 9.3 Writing Context

Blocks write to context via two fields:

- **`set_context`** — writes to function context variables. Keys must be declared in the function's `context`.
- **`set_workflow`** — writes to workflow context variables. Keys must be declared in the workflow's `context`.

```yaml
set_context:
  attempts: "context.attempts + 1"
  best_score: "output.score > context.best_score ? output.score : context.best_score"
set_workflow:
  total_calls: "workflow.total_calls + 1"
```

**Evaluation rules:**

1. Both `set_context` and `set_workflow` expressions are evaluated after the block produces output.
2. Expressions have access to `output`, `input`, `context`, `workflow`, and `blocks.*` (same scope as the block's templates, plus `output`).
3. Expressions within each field are evaluated **atomically** — all expressions see the state from *before* the write, not partially-updated state.
4. `set_context` writes are applied first, then `set_workflow` writes. Transitions are evaluated after both complete.
5. **Undeclared variable:** A key in `set_context` that is not declared in the function's `context`, or a key in `set_workflow` that is not declared in `workflow.context`, is a **load-time error**.
6. **Type mismatch:** If static analysis can determine that the expression produces a type incompatible with the declaration, it is a load-time **warning**.

### 9.4 Context in Cycles

Context is the primary mechanism for bounding and controlling cycles. Since variables are pre-declared with initial values, cycle patterns are straightforward:

**Retry counter pattern:**

```yaml
attempt:
  prompt: "Attempt to solve: {{input.problem}}"
  schema:
    type: object
    required: [solution, confidence]
    properties:
      solution: { type: string }
      confidence: { type: number }
  set_context:
    attempts: "context.attempts + 1"
  transitions:
    - when: 'output.confidence >= 0.9'
      goto: done
    - when: 'context.attempts < 3'
      goto: attempt
    - goto: done
```

**Accumulator pattern:**

```yaml
set_context:
  all_results: "context.all_results + [output]"
```

**Best-so-far pattern:**

```yaml
set_context:
  best_score: "output.score > context.best_score ? output.score : context.best_score"
```

No `has()` guards needed — `context.attempts` starts at `0`, `context.all_results` starts at `[]`, `context.best_score` starts at `0.0`.

### 9.5 Context and Compaction

When conversation compaction fires (§4.6), context is **not** compacted — it retains its full state. Context is separate from conversation history. The compaction summary may reference context values (e.g., "after 3 attempts, best score was 0.87"), but context variables are preserved verbatim.

This means context is suitable for data that must survive compaction: running totals, best-so-far results, iteration counters. Conversation history degrades gracefully through summarization; context does not degrade.

### 9.6 Context and Parallel Execution

Parallel function calls (via `parallel: all|any|n_of_m`) each get their own function context (they're separate function invocations). However, parallel functions **share** the workflow context. Writes to workflow context from parallel functions are applied in completion order. If two parallel functions write the same workflow variable, the last to complete wins. Load-time validation flags this as a **warning**.

The parent block's `set_context` and `set_workflow` run after the parallel calls complete, in the parent function's scope.

Dataflow blocks within a single function share the function's context. Today the scheduler runs blocks within a topological level sequentially, so write ordering is deterministic (declaration order). Within-level parallelism is future work; the load-time **warning** when two sibling blocks write the same context variable is preserved as future-proofing for that work — once parallel execution lands, two sibling writes to the same variable would resolve in completion order (nondeterministic).

## 10. Validation & Error Handling

Validation is split into two phases: **load-time** (static, before any block executes) and **runtime** (during execution).

### 10.1 Load-Time Validation

The loader performs all of the following checks when a workflow file is loaded. Any error prevents execution. Warnings are reported but do not prevent execution.

#### Structural Checks

| Check | Severity | Description |
|---|---|---|
| YAML parse | Error | Workflow file must be valid YAML. |
| Required top-level fields | Error | `functions` map must exist and be non-empty. |
| Block type discrimination | Error | Each block must have exactly one of `prompt` or `call`. |
| Field validity | Error | No forbidden fields per block type (§5.3). |
| Block name format | Error | Names match `[a-z][a-z0-9_]*`, no reserved names. |
| Block name uniqueness | Error | No duplicate names within a function. |
| Function name uniqueness | Error | No duplicate function names within a workflow file. |
| Terminal block validation | Error | If `terminals` is declared, listed blocks must exist and have no outgoing transitions. |
| `n_of_m` requires `n` | Error | `parallel: n_of_m` without an `n` field. |
| `n` range | Error | `n` must be `1 <= n <= len(call list)`. |
| Context variable declaration | Error | Each entry in `workflow.context` and `function.context` must have `type` and `initial` fields. `type` must be a valid JSON Schema type. `initial` must be compatible with `type`. |
| Context variable names | Error | Context variable names must match `[a-z][a-z0-9_]*`. |
| `set_context` target validity | Error | Every key in `set_context` must be declared in the function's `context`. |
| `set_workflow` target validity | Error | Every key in `set_workflow` must be declared in `workflow.context`. |

#### Graph Checks

| Check | Severity | Description |
|---|---|---|
| Dataflow cycle detection | Error | `depends_on` edges must form a DAG. Cycles in data edges are forbidden. |
| Transition target existence | Error | Every `goto` target must name a block in the same function. |
| Call target existence | Error | Every `call` target must name a function in the workflow (or a registered external function). |
| Unreachable blocks | Warning | Blocks with no inbound edges (control or data) and that are not entry points are never executed. |
| Dead transitions | Warning | Transitions after an unconditional fallback are unreachable. |
| Parallel context write conflict | Warning | Two blocks that may execute in parallel both write the same `set_context` or `set_workflow` variable. |

#### Type Checks

| Check | Severity | Description |
|---|---|---|
| Schema validity | Error | All schemas parse as valid JSON Schema. Root type is `object`. `required` is non-empty. |
| External schema resolution | Error | All `$ref:` paths resolve to existing files. Workflow-level `$ref:#name` references exist in `schemas` map. |
| Template reference resolution | Error | `{{blocks.<name>.output.field}}` — the named block exists, the field exists in its schema. |
| Template reference reachability | Error | The referenced block is guaranteed to have executed before the referencing block (domination or `depends_on`). |
| CEL compilation | Error | All `when` guards and `set_context` expressions compile as valid CEL. |
| CEL variable scope | Error | All CEL expressions (guards, `set_context`, template `{{...}}`) only reference variables available in their evaluation context (§6.3). |
| CEL optional field safety | Error | CEL expressions in `when` guards, `set_context`, and `set_workflow` that access fields not in the source schema's `required` list without a `has()` guard are rejected. The check walks the parsed CEL AST (`cel_parser::parse`), extracts `Member`/`Ident` field-access paths, resolves each path's root namespace (`output`, `input`, `context`, `workflow`) and schema, and verifies that any field not in `required` is accessed only inside a `has()` call or behind a `has()` check in a short-circuit expression (e.g., `has(output.x) && output.x > 0`). |
| Agent model resolution | Error | All `agent.model` fields (block, function, workflow) resolve via flick's `ModelRegistry`. |
| Agent grant validity | Error | `agent.grant` values must be valid: `tools`, `write`, `network`. |
| Agent grant normalization | — | `write` and `network` imply `tools` (auto-added if missing). Specifying `tools` (custom tool names) implies `grant: [tools]`. |
| Agent write_paths without write grant | Warning | `agent.write_paths` is specified but `agent.grant` does not include `write`. |
| Agent extends resolution | Error | `agent.extends` target must exist in workflow-level `agents` map. |
| Agent `$ref:#name` resolution | Error | `agent: "$ref:#name"` target must exist in workflow-level `agents` map. |
| Agent `$ref:path` resolution | Error | `agent: "$ref:path"` file must exist and parse as a valid agent config. |
| Agent `extends` cycle | Error | Circular `extends` chains in named agent configs (e.g., A extends B extends A). |
| Input schema match | Error | Call block `input` fields must provide all `required` fields declared in the called function's `input` schema. For per-call lists, each entry's `input` is validated against its own target function's schema. |
| Per-call list consistency | Error | A per-call list block must not have a block-level `input`. A uniform-list or single-function block must have a block-level `input`. Per-call entries must each have `fn` and `input` fields. |
| Function output schema | Error | If `output` is an explicit schema: must be valid JSON Schema with root type `object`. Terminal block schemas must be compatible with the declared output schema. |
| Function output inference | Error | If `output` is `infer` or omitted: at least one terminal block must be detectable. If no terminals found, the author must declare an explicit output schema. |
| Call block output type checking | Warning | Call block `output` mapping fields and downstream `{{blocks.<name>.output.*}}` references are checked against the called function's output schema (explicit or inferred). |

### 10.2 Runtime Errors

Runtime errors occur during block execution. The error handling strategy depends on the error type.

#### Schema Validation Failure

The LLM returned output that does not validate against the block's JSON Schema.

- **Behavior:** The block fails. Error message includes the block name, the schema violation details (field, expected type, actual value), and the raw LLM output.
- **Propagation:** The block is marked failed. If the block is on a control-flow path, the function fails. If cue orchestration wraps this function, cue's retry/escalation handles the failure (§11).

#### Guard Evaluation Error

A CEL guard expression raises a runtime error (e.g., type mismatch, division by zero).

- **Behavior:** The block fails. The error message includes the block name, the guard expression, and the CEL error.
- **Propagation:** Block failure → function failure → cue retry/escalation if orchestrated.

**Rationale:** Mech's static validation (§10.1) catches the vast majority of guard errors at load time — variable scope, field existence, type compatibility, and optional-field safety (`has()` requirement). A guard error that survives load-time checks indicates a bug in the workflow or the executor, not a recoverable condition. Failing loudly makes these bugs visible immediately rather than masking them behind a silent false evaluation and unpredictable control flow.

#### Template Resolution Error

A `{{...}}` reference in a prompt or input mapping cannot be resolved (undefined field, out-of-bounds array index, null intermediate).

- **Behavior:** The block fails immediately (the prompt cannot be rendered).
- **Propagation:** Same as schema validation failure — block failure, function failure.

#### LLM Call Failure

The underlying flick call fails (network error, rate limit, provider error).

- **Behavior:** The block fails with the flick error message.
- **Propagation:** Block failure → function failure → cue retry/escalation if orchestrated.

#### Timeout

Per-block timeout (if configured at the executor level) exceeded.

- **Behavior:** The in-flight LLM call is cancelled. The block fails with a timeout error.
- **Propagation:** Block failure → function failure.

#### Cancellation (Parallel Calls)

When `any` or `n_of_m` completes early, remaining functions receive cancellation.

- **Behavior:** Cancelled functions stop executing. Their output is absent.
- **Propagation:** Not a failure — this is normal parallel-call behavior. Template references to cancelled functions' outputs are runtime errors (§4.4).

### 10.3 Error Reporting

All errors (load-time and runtime) include:

- **Location:** Workflow file path, function name, block name, field name.
- **Context:** The expression or schema that failed, the actual value (for runtime errors).
- **Structured format:** Errors are returned as typed Rust values, not just strings. The executor, CLI, and TUI can render them appropriately.

## 11. Integration with Cue

Mech does not replace cue — it provides a declarative way to define what a cue task does internally. A mech workflow function is an **implementation strategy** for a `TaskNode`, not a replacement for the orchestration protocol.

### 11.1 Mapping: Function = Leaf Task Implementation

A cue leaf task can be implemented by a mech workflow function instead of a direct agent call. When the orchestrator calls `execute_leaf()` on such a task, the task's implementation loads and executes a mech workflow function, returning the function's structured output as the leaf result.

```
Cue Orchestrator
  └── execute_task(root)
        └── assess → Branch
              └── decompose → [subtask_1, subtask_2, subtask_3]
                    ├── execute_task(subtask_1)
                    │     └── assess → Leaf
                    │           └── execute_leaf()
                    │                 └── mech function: "triage_workflow"
                    │                       ├── block: classify
                    │                       ├── block: analyze
                    │                       └── block: respond  ← output
                    ├── execute_task(subtask_2) ...
                    └── execute_task(subtask_3) ...
```

**The mech function is opaque to cue.** Cue sees a leaf task that produces a `TaskOutcome`. The function's internal blocks, transitions, context, and conversation are invisible to the orchestrator. The function either succeeds (returning structured output) or fails (returning a failure reason).

### 11.2 Mapping: Each Block is NOT a Cue Task

Individual blocks are not modeled as cue tasks. This is deliberate:

- **Granularity mismatch.** Cue tasks have assessment, decomposition, verification, fix loops, recovery. Blocks are simpler — prompt → output → transition. Wrapping each block as a cue task would impose unnecessary overhead and state machine complexity.
- **Conversation continuity.** Blocks within a function share a conversation (§4.6). Cue tasks are independent — they don't share conversation state. Making each block a separate task would break the conversation model.
- **Retry semantics differ.** Cue's retry escalates models (Haiku → Sonnet → Opus). Block-level retry is a self-loop with accumulated context. These are different mechanisms for different purposes.

**The mech executor is a separate execution engine** that runs inside `execute_leaf()`. It handles block scheduling, template resolution, LLM calls, conversation management, and context mutation. Cue handles the outer loop: assessment, decomposition, verification, fix loops, recovery.

### 11.3 Mech-Backed TaskNode

A `TaskNode` implementation that executes mech workflow functions needs:

```
MechTask {
    // Standard cue fields
    id: TaskId,
    phase: TaskPhase,
    goal: String,
    ...

    // Mech-specific
    function_name: String,          // which mech function to execute
    workflow: Arc<LoadedWorkflow>,   // parsed + validated workflow (shared across tasks)
    input: serde_json::Value,       // input arguments for the function
}
```

**`execute_leaf` implementation:**

1. Look up the function by name in the loaded workflow.
2. Create a new `FunctionExecution` (conversation, context, block states).
3. Run the mech executor (§4.3 activation rules, §4.6 conversation model).
4. On success: return `TaskOutcome::Success`. The function's output is stored for the parent to access.
5. On failure (any block fails, template error, schema error): return `TaskOutcome::Failed { reason }`.

**`assess` implementation:** Mech-backed tasks are always leaves. The `assess` method returns `AssessmentResult { path: Leaf, model: <workflow agent default model>, ... }`.

**`verify_branch` / `decompose`:** Not applicable — mech-backed tasks are leaves. These methods should not be called on a mech task. If they are (implementation error), they return an error.

### 11.4 Cue Retry and Mech Failures

When a mech function fails (e.g., schema validation failure on a block), cue's outer loop handles retry:

1. `execute_leaf()` returns `TaskOutcome::Failed { reason: "Block 'analyze' schema validation failed: ..." }`.
2. Cue's retry mechanism re-calls `execute_leaf()` — possibly with an escalated model.
3. The mech function re-executes from scratch (fresh conversation, fresh context). Model escalation from cue overrides the workflow/function-level default agent model, but block-level agent config is never overridden.

**Escalation interaction with agent configuration:**

| Level | Source | Overridden by cue escalation? |
|---|---|---|
| Workflow default | `workflow.agent.model` | Yes |
| Function default | `function.agent.model` | Yes |
| Block override | `block.agent.model` | No — block-level is intentional (e.g., cheap model for triage, expensive model for synthesis) |
| Grant/tools/write_paths | Any level | No — cue escalation only affects model selection, not runtime permissions |

### 11.5 Workflow as Branch Task

An alternative mapping: a cue branch task uses a mech workflow function for its **decomposition** — the function's blocks define the subtask structure rather than executing directly.

This is a future extension. The current design maps mech functions to leaf execution only. Branch decomposition remains the domain of agent calls (via `TaskNode::decompose`). If this extension proves valuable, a mech function's terminal blocks would produce `SubtaskSpec` values instead of free-form output.

### 11.6 Workflow Loading and Sharing

Workflows are loaded once and shared across tasks:

1. **Load phase:** Parse YAML, validate (§10.1), compile CEL expressions, resolve schemas. Produce a `LoadedWorkflow` (immutable, `Arc`-wrapped).
2. **Execution phase:** Each `execute_leaf()` call creates a fresh `FunctionExecution` referencing the shared `LoadedWorkflow`. No re-parsing or re-validation.
3. **Registration:** Workflows are registered in the `TaskStore` (or a workflow registry accessible to it) so that mech-backed tasks can reference them by name.

### 11.7 Event Integration

Mech block execution emits events that map to the application's event system:

| Mech event | Description |
|---|---|
| `BlockStarted { function, block }` | Block execution begins. |
| `BlockCompleted { function, block, output }` | Block produced valid output. |
| `BlockFailed { function, block, error }` | Block failed (schema, template, LLM error). |
| `TransitionFired { function, from, to, guard }` | A transition matched and fired. |
| `CompactionTriggered { function, tokens_before, tokens_after }` | Conversation compaction occurred. |

These are application-level events (defined in the application crate, not in cue). They supplement cue's `CueEvent` variants, which cover orchestration-level events (task lifecycle, fix loops, recovery).

## 12. YAML Reference Grammar

Complete annotated schema for the workflow file format. All fields shown; optional fields marked with `# optional`.

```yaml
# ─── Top-Level ───────────────────────────────────────────────

workflow:                                       # optional — workflow-level defaults
  system: <string>                              # optional — default system prompt (template vars allowed)

  agent: <object | "$ref:#name" | "$ref:path">  # optional — default agent config (§5.5)
  #   model: <string>                           #   flick model name
  #   grant: [tools, write, network]            #   ToolGrant flags
  #   tools: [<tool_name>, ...]                 #   custom tool names (registered by executor)
  #   write_paths: [<path>, ...]                #   writable paths (relative to project root)
  #   timeout: <string>                         #   agent run timeout (e.g., "30s", "5m")
  #   extends: <agent_name>                     #   base named config to extend

  agents:                                       # optional — named agent configurations (like schemas)
    <agent_name>:
      model: <string>                           # optional
      grant: [<grant>, ...]                     # optional
      tools: [<tool_name>, ...]                 # optional
      write_paths: [<path>, ...]                # optional
      timeout: <string>                         # optional

  context:                                      # optional — workflow-level context variables
    <variable_name>:                            #   identifier: [a-z][a-z0-9_]*
      type: <string>                            #   JSON Schema type
      initial: <value>                          #   literal initial value
  schemas:                                      # optional — reusable schema definitions
    <schema_name>:
      type: object
      required: [...]
      properties: { ... }
  compaction:                                   # optional — default compaction config
    keep_recent_tokens: <integer>               #   tokens of recent history to preserve
    reserve_tokens: <integer>                   #   trigger threshold: used > context_window - reserve
    fn: <string>                                # optional — custom compaction function name

# ─── Functions ───────────────────────────────────────────────

functions:
  <function_name>:                              # identifier: [a-z][a-z0-9_]*

    input:                                      # required — JSON Schema for function arguments
      type: object
      required: [...]
      properties: { ... }

    output: <object | "$ref:path" | "$ref:#name" | "infer">  # optional — output schema (default: infer)

    system: <string>                            # optional — override workflow system prompt
    agent: <object | "$ref:#name" | "$ref:path">  # optional — override workflow agent config (§5.5)
    terminals: [<block_name>, ...]              # optional — explicit terminal blocks (auto-detected if omitted)

    context:                                    # optional — function-level context variables
      <variable_name>:                          #   identifier: [a-z][a-z0-9_]*
        type: <string>                          #   JSON Schema type
        initial: <value>                        #   literal initial value

    compaction:                                 # optional — override workflow compaction config
      keep_recent_tokens: <integer>
      reserve_tokens: <integer>
      fn: <string>                              # optional

    # ─── Blocks ────────────────────────────────────────────

    blocks:
      # ── Prompt Block ──
      <block_name>:                             # identifier: [a-z][a-z0-9_]*, not input/output/context
        prompt: <string>                        # required — template string ({{CEL}} expressions)
        schema: <object | "$ref:path" | "$ref:#name">  # required — output JSON Schema
        agent: <object | "$ref:#name" | "$ref:path">   # optional — override function/workflow agent config (§5.5)

        depends_on: [<block_name>, ...]         # optional — data edges (must be acyclic)

        set_context:                            # optional — write to function context (declared vars only)
          <key>: <cel_expression>               #   evaluated against output, input, context, workflow
        set_workflow:                           # optional — write to workflow context (declared vars only)
          <key>: <cel_expression>

        transitions:                            # optional — outbound control edges
          - when: <cel_expression>              # optional — CEL guard (omit for unconditional)
            goto: <block_name>                  # required — target block in same function

      # ── Call Block (single function or uniform list) ──
      <block_name>:
        call: <string | [string, ...]>          # required — function name(s)
        input:                                  # required — shared input mapping
          <field>: <template_or_cel_expr>

        output:                                 # optional — construct block output from results
          <field>: <template_or_cel_expr>       #   refs: <fn_name>.output.*, input, context
        parallel: <all | any | n_of_m>          # optional — join strategy (list calls only)
        n: <integer>                            # optional — required when parallel: n_of_m

        depends_on: [<block_name>, ...]         # optional
        set_context:                            # optional
          <key>: <cel_expression>
        set_workflow:                           # optional
          <key>: <cel_expression>
        transitions:                            # optional
          - when: <cel_expression>              # optional
            goto: <block_name>                  # required

      # ── Call Block (per-call list — heterogeneous inputs) ──
      <block_name>:
        call:                                   # required — list of { fn, input } entries
          - fn: <string>                        #   function name
            input:                              #   input mapping for this call
              <field>: <template_or_cel_expr>
          - fn: <string>
            input:
              <field>: <template_or_cel_expr>
        # no block-level input — each entry carries its own

        output:                                 # optional — construct block output from results
          <field>: <template_or_cel_expr>       #   refs: <fn_name>.output.*, input, context
        parallel: <all | any | n_of_m>          # optional — join strategy
        n: <integer>                            # optional — required when parallel: n_of_m

        depends_on: [<block_name>, ...]         # optional
        set_context:                            # optional
          <key>: <cel_expression>
        set_workflow:                           # optional
          <key>: <cel_expression>
        transitions:                            # optional
          - when: <cel_expression>              # optional
            goto: <block_name>                  # required
```

### 12.1 Field Type Summary

| Field | Type | Where |
|---|---|---|
| `workflow.system` | Template string | Top-level |
| `workflow.agent` | Agent config object or `$ref` string | Top-level (optional) |
| `workflow.agents` | Map of name → agent config object | Top-level (optional) |
| `workflow.context` | Map of variable name → `{ type, initial }` | Top-level (optional) |
| `workflow.schemas` | Map of name → JSON Schema object | Top-level |
| `workflow.compaction` | Compaction config object | Top-level |
| `function.input` | JSON Schema (root type: object) | Function |
| `function.output` | JSON Schema object, `$ref` string, or `"infer"` | Function (optional, default: `infer`) |
| `function.system` | Template string | Function |
| `function.agent` | Agent config object or `$ref` string | Function (optional) |
| `function.terminals` | List of block name strings | Function |
| `function.context` | Map of variable name → `{ type, initial }` | Function (optional) |
| `function.compaction` | Compaction config object | Function |
| `block.prompt` | Template string | Prompt block |
| `block.schema` | JSON Schema object or `$ref` string | Prompt block |
| `block.agent` | Agent config object or `$ref` string | Prompt block (optional) |
| `agent.model` | Model name string | Agent config |
| `agent.grant` | List of grant strings (`tools`, `write`, `network`) | Agent config (optional) |
| `agent.tools` | List of tool name strings | Agent config (optional) |
| `agent.write_paths` | List of path strings | Agent config (optional) |
| `agent.timeout` | Duration string (e.g., `"30s"`) | Agent config (optional) |
| `agent.extends` | Agent name string | Agent config (optional, inline only) |
| `block.call` | String, list of strings, or list of call entries | Call block |
| `block.input` | Map of field → template/CEL expression | Call block (single/uniform only) |
| `block.output` | Map of field → template/CEL expression | Call block (optional) |
| `call_entry.fn` | String | Per-call list entry |
| `call_entry.input` | Map of field → template/CEL expression | Per-call list entry |
| `block.parallel` | Enum: `all`, `any`, `n_of_m` | Call block |
| `block.n` | Positive integer | Call block |
| `block.depends_on` | List of block name strings | Any block |
| `block.set_context` | Map of variable name → CEL expression | Any block (function context only) |
| `block.set_workflow` | Map of variable name → CEL expression | Any block (workflow context only) |
| `block.transitions` | List of transition entries | Any block |
| `transition.when` | CEL expression string | Transition |
| `transition.goto` | Block name string | Transition |

### 12.2 Complete Example

```yaml
workflow:
  system: "You are a customer support agent."

  agents:
    default:
      model: sonnet
      grant: [tools]
    diagnostician:
      model: opus
      grant: [tools, network]
      tools: [web_search]

  agent: "$ref:#default"

  schemas:
    resolution:
      type: object
      required: [resolved, summary]
      properties:
        resolved: { type: boolean }
        summary: { type: string }

functions:
  support_triage:
    input:
      type: object
      required: [ticket_text]
      properties:
        ticket_text: { type: string }
        customer_tier: { type: string, enum: [free, pro, enterprise] }

    context:
      attempts: { type: integer, initial: 0 }

    blocks:
      classify:
        prompt: |
          Classify the following support ticket into a category.
          Ticket: {{input.ticket_text}}
        schema:
          type: object
          required: [category, urgency]
          properties:
            category: { type: string, enum: [billing, technical, account, other] }
            urgency: { type: string, enum: [low, medium, high] }
        transitions:
          - when: 'output.category == "billing"'
            goto: billing
          - when: 'output.category == "technical"'
            goto: technical
          - goto: general

      billing:
        call: resolve_billing
        input:
          issue: "{{input.ticket_text}}"
          urgency: "{{blocks.classify.output.urgency}}"
        depends_on: [classify]
        transitions:
          - goto: respond

      technical:
        agent: "$ref:#diagnostician"
        prompt: |
          Diagnose this technical issue.
          Ticket: {{input.ticket_text}}
          Urgency: {{blocks.classify.output.urgency}}
        schema:
          type: object
          required: [diagnosis, steps]
          properties:
            diagnosis: { type: string }
            steps: { type: array, items: { type: string } }
        depends_on: [classify]
        set_context:
          attempts: "context.attempts + 1"
        transitions:
          - when: 'size(output.steps) > 0'
            goto: respond
          - when: 'context.attempts < 3'
            goto: technical
          - goto: escalate

      general:
        prompt: |
          Provide a helpful response to this general inquiry.
          Ticket: {{input.ticket_text}}
        schema: "$ref:#resolution"
        depends_on: [classify]

      escalate:
        prompt: |
          This ticket could not be resolved automatically after {{context.attempts}} attempts.
          Draft an escalation notice for a human agent.
          Ticket: {{input.ticket_text}}
          Last diagnosis: {{blocks.technical.output.diagnosis}}
        schema:
          type: object
          required: [notice, suggested_team]
          properties:
            notice: { type: string }
            suggested_team: { type: string }

      respond:
        prompt: |
          Write a customer-facing response summarizing the resolution.
          Customer tier: {{input.customer_tier}}
        schema: "$ref:#resolution"

  resolve_billing:
    input:
      type: object
      required: [issue, urgency]
      properties:
        issue: { type: string }
        urgency: { type: string }
    system: "You are a billing specialist. Be precise about amounts and dates."
    agent:
      extends: default
      grant: [write]
      write_paths: [billing/]

    blocks:
      analyze:
        prompt: |
          Analyze this billing issue: {{input.issue}}
          Urgency: {{input.urgency}}
        schema:
          type: object
          required: [root_cause, resolution_action]
          properties:
            root_cause: { type: string }
            resolution_action: { type: string }

      resolve:
        prompt: |
          Apply the resolution: {{blocks.analyze.output.resolution_action}}
          Confirm the outcome.
        schema:
          type: object
          required: [resolved, summary]
          properties:
            resolved: { type: boolean }
            summary: { type: string }
        depends_on: [analyze]
```

---

## 13. Implementation Plan

This plan breaks mech implementation into incremental, independently-testable deliverables. Each deliverable will be implemented by a Claude Code agent following strict TDD discipline:

**Per-deliverable TDD cycle:**
1. **Write tests first.** Cover the deliverable's acceptance criteria as failing unit tests (and integration tests where applicable). Tests must exercise real behavior — no silent skipping, no mocking internal mech types (see CLAUDE.md).
2. **Implement** the minimum code needed to make the tests pass. Prefer the simplest design that satisfies the contract.
3. **Verify.** Run `cargo test -p mech`, `cargo clippy -p mech --all-targets -- -D warnings`, and `cargo fmt --check`. All must pass.
4. **Review.** Run the `/review` slash command on the uncommitted changes (launches the 7-lens review agent fleet in parallel), then `/triage` and `/fix` any MUST-FIX findings. Re-run tests after fixes.
5. **Update STATUS.md** — mark the deliverable complete, move to the next.

Each deliverable should end in a commit with tests passing and review clean. Later deliverables depend on earlier ones; they must not be parallelized across agents without respecting the dependency order listed below.

---

### Deliverable 1 — Crate skeleton & error types

**Scope:** Create the `mech` crate. Declare dependencies (`cue`, `reel`, `cel-interpreter`, `serde`, `serde_yaml`, `schemars`, `thiserror`, `tokio`). Define the public error enum covering the 5 runtime error categories (§10) and placeholder load-time error variants. Define `MechResult<T>`. No logic yet — just the module shell and error surface.

**Tests first:**
- `error_display_formats_correctly` — each error variant has a human-readable Display impl.
- `error_is_send_sync` — compile-time check that errors can cross task boundaries.
- `crate_builds_clean` — implicit (clippy/fmt).

**Implement:** `src/lib.rs`, `src/error.rs`, `Cargo.toml`.

**Acceptance:** Crate builds, clippy clean, error types documented.

---

### Deliverable 2 — YAML schema types (parse-only, no validation)

**Scope:** Define the serde structs that mirror the §12 YAML grammar: `MechDocument`, `FunctionDef`, `BlockDef` (enum: `Prompt`, `Call`), `AgentConfig`, `TransitionDef`, `SchemaRef`, `ContextVarDef`, etc. Use `#[serde(deny_unknown_fields)]`. Parse only — no semantic validation. Support the three `call.input` forms (string, uniform list, per-call object list) via an untagged enum.

**Tests first:**
- Parse the §12 worked example end-to-end; assert top-level structure matches.
- Parse each `call.input` form variant.
- Parse schema `$ref` vs inline vs `infer`.
- Parse agent config cascade at all three levels (workflow/function/block).
- Round-trip: parse → re-serialize → parse again yields equal struct.
- Reject unknown fields with a clear error.
- Parse every field documented in §5 and §9 at least once.

**Implement:** `src/schema/mod.rs` and submodules.

**Acceptance:** All YAML forms from §12 deserialize; unknown fields rejected.

---

### Deliverable 3 — CEL expression compilation & evaluation

**Scope:** Wrap `cel-interpreter` with mech's five namespaces (`input`, `context`, `workflow`, `block`, `meta`). Provide `CelExpression` (compiled) and `CelEvaluator` (binds namespaces → evaluates). Support `{{...}}` template interpolation in strings: split string into literal + expression segments, evaluate, concatenate. Evaluate bare expressions for guards and `set_*` RHS.

**Tests first:**
- Compile valid CEL expressions (arithmetic, field access, method calls).
- Reject invalid CEL at compile time with source-location error.
- Evaluate with bound namespaces; each namespace independently accessible.
- Template interpolation: `"hello {{input.name}}"` with `name="world"` → `"hello world"`.
- Template with multiple expressions, escaped braces, nested field access.
- Guard evaluation returning non-bool → error.
- Missing namespace field → clear error naming the path.
- Type coercion rules (string/number/bool) match CEL spec.

**Implement:** `src/cel.rs`.

**Acceptance:** CEL compiles once per workflow load; evaluation is a pure function of namespace bindings.

---

### Deliverable 4 — Schema registry & JSON Schema handling

**Scope:** Resolve `$ref:#name` against workflow-level `schemas` map. Support inline schemas and `infer` placeholder (inference deferred to deliverable 6). Validate JSON values against resolved schemas using `jsonschema` crate. Provide `SchemaRegistry` with `resolve(ref) → Schema` and `validate(schema, value) → Result`.

**Tests first:**
- Resolve `$ref:#name` to inline schema.
- Unresolved `$ref` → load-time error naming the missing schema.
- Validate a value against a resolved schema (pass + fail cases).
- Validation error includes JSON path to the failing field.
- Circular `$ref` detected and rejected.
- `infer` placeholder accepted at parse, flagged for later resolution.

**Implement:** `src/schema/registry.rs`.

**Acceptance:** Every `$ref` in a workflow resolves or errors at load time.

---

### Deliverable 5 — Load-time validation (the 24+ checks)

**Scope:** Implement the load-time validation pass enumerated in §10. Walks the parsed `MechDocument` and emits the complete list of errors (not just the first). Checks include: unique block IDs, transition targets exist, guards are valid CEL, `set_*` targets are declared variables, agent configs reference declared agents, schema refs resolve, call blocks reference declared functions, terminal blocks have no outgoing transitions, workflow has at least one entry, etc.

**Tests first:**
- Each of the 24+ checks has at least one failing fixture and one passing fixture.
- Multiple errors in one workflow → all reported in one pass.
- The §12 worked example validates clean.
- Error messages include source location (file + block ID).

**Implement:** `src/validate.rs`.

**Acceptance:** Invalid workflows fail fast at load time with a complete error list.

---

### Deliverable 6 — Schema inference for function outputs

**Scope:** When a function declares `output: infer`, derive the schema by walking backward from terminal blocks and unioning their output schemas. Error if terminal blocks have incompatible schemas.

**Tests first:**
- Single terminal block → function output schema equals block output.
- Multiple terminal blocks with identical schemas → unified schema.
- Incompatible terminal schemas → error.
- Inference interacts correctly with `$ref` and inline schemas.
- Inference is idempotent (running twice yields same result).

**Implement:** `src/schema/infer.rs`; wire into load pipeline after validation.

**Acceptance:** Every function has a concrete output schema after loading.

**Design notes (implemented):**
- **Option A (keyed map) — C-07 fix:** Multi-terminal output shape now depends on execution mode. Dataflow sinks produce a keyed-map schema `{type: object, required: [t1, t2, …], properties: {t1: s1, t2: s2, …}}`, matching the runtime’s `{terminal_name: output}` collection. CFG paths require structural equality across all terminals (unchanged from original); non-matching terminals error rather than synthesise a `oneOf`.
- Terminal detection is mode-aware: dataflow terminals are sink nodes (no transitions AND not depended upon by any other block); imperative terminals are blocks with no outgoing transitions.
- Terminal prompt blocks contribute their resolved `schema:` (inline or `$ref:#name`). Terminal call blocks contribute the callee's output schema only when the block is a single-function call with no `output:` mapping; list-form calls or call blocks that declare an `output:` mapping cannot be structurally inferred and must live under a function with an explicit `output:` schema.
- A fixed-point pass resolves chains of `infer` functions (A's terminal calls B, B's output also `infer`). Functions still unresolved after the fixed point error out.
- Inference mutates the parsed `MechDocument` in place, replacing `SchemaRef::Infer` with `SchemaRef::Inline`, and is idempotent on a second run. Public entry point: `mech::infer_function_outputs(&mut MechDocument)`.

---

### Deliverable 7 — Workflow loader (end-to-end load pipeline)

**Scope:** Public `WorkflowLoader::load(path) → Workflow`. Composes parse → resolve schemas → validate → infer → compile CEL. Produces an immutable `Workflow` struct ready for execution. No execution yet.

**Tests first:**
- Load the §12 worked example; assert function count, block count, agents present.
- Load failure on missing file, bad YAML, semantic errors — each yields the right error variant.
- Loaded `Workflow` is `Send + Sync` (multi-threaded execution precondition).
- Loading is deterministic (same input → same output).

**Implement:** `src/loader.rs`.

**Acceptance:** One call turns a YAML file into a fully-validated, ready-to-run `Workflow`.

---

### Deliverable 8 — Context & state management

**Scope:** Runtime state for a single function invocation. `ExecutionContext` holds: declared `workflow.*` variables (shared, mutex-guarded across functions), declared `context.*` variables (per-invocation), `block.*` outputs keyed by block ID, `input` (function input), `meta` (runtime info). Implements `set_context` / `set_workflow` writes with type checking against declarations.

**Tests first:**
- Declare variables with initial values; read back.
- `set_context` assigns a CEL-evaluated value; type-checked against declaration.
- `set_context` to undeclared variable → error.
- `set_workflow` writes visible across concurrent function invocations.
- Reading a block's output before it runs → error.
- Namespace bindings produced by `ExecutionContext` match CEL evaluator expectations.

**Implement:** `src/context.rs`.

**Acceptance:** Context passes CEL evaluator round-trips cleanly.

---

### Deliverable 9 — Prompt block executor

**Scope:** Execute a single `prompt` block: resolve its agent config (cascade), build a reel `Agent`, render the prompt template, invoke the agent with the declared output schema, store the result in `block.<id>`. Use structured output via reel's schema support. Does *not* yet handle transitions.

**Tests first:**
- Execute a trivial prompt block against a mock `reel::Agent`; assert output stored in context.
- Agent config cascade (workflow → function → block) produces the right effective config.
- Prompt template interpolation evaluates CEL against current context.
- Output schema mismatch → runtime error.
- Tool grants and `write_paths` passed through to reel.

**Implement:** `src/exec/prompt.rs`. Use a test-only `AgentExecutor` trait to inject a fake agent without mocking reel internals — the trait lives in mech and has one real impl (reel) + one test impl.

**Acceptance:** Prompt blocks produce schema-conformant outputs.

---

### Deliverable 10 — Call block executor

**Scope:** Execute a `call` block. Resolve called function(s), build per-call input via the input mapping (three forms), invoke the function via a `FunctionExecutor` callback (supplied by the workflow executor in the next deliverable), apply output mapping to produce the block's output. Callee starts with empty conversation history (conversation-transparent).

**Tests first:**
- Single string form: `call: fn_name` with shared input.
- Uniform list: `call: [a, b, c]` all receive same input.
- Per-call list: `call: [{fn: a, input: ...}, {fn: b, input: ...}]` heterogeneous.
- Output mapping produces expected block output from collected function results.
- Calling an undeclared function → error (but this should already be caught at load time; runtime check is defense in depth).
- Callee's conversation does not leak to caller.

**Implement:** `src/exec/call.rs`.

**Acceptance:** All three input forms execute correctly; output mapping works.

---

### Deliverable 11 — Transitions & block scheduling (imperative mode)

**Scope:** Given a function's current block, evaluate outgoing transitions in declared order, pick the first whose guard is true (or the unconditional fallback), advance to the next block. Detect terminal blocks. Implement self-loops. Ordered evaluation stops at first match.

**Tests first:**
- Linear sequence: block A → B → C terminates at C.
- Guard selects among multiple outgoing transitions in declared order.
- Self-loop executes the same block until guard flips.
- Terminal block ends function execution.
- No matching transition and no fallback → runtime error.
- Cycles without self-loop annotation → caught at load time (validation deliverable 5).

**Implement:** `src/exec/schedule.rs`.

**Acceptance:** Imperative-mode functions run to completion.

---

### Deliverable 12 — Function executor & workflow runtime

**Scope:** `FunctionExecutor` runs a single function invocation: initializes context, starts at entry block, drives the block→transition loop, returns the function output (per declared/inferred schema). `WorkflowExecutor` orchestrates the top-level entry function and wires `FunctionExecutor` into call blocks (satisfying deliverable 10's callback). Handles both `imperative` and `dataflow` workflow modes — dataflow mode builds a DAG from `depends_on`, topo-sorts, executes level by level (§12 worked example).

**Tests first:**
- Run the §12 worked example end-to-end with mock agents; assert final output.
- Imperative mode: single function, linear flow.
- Imperative mode: function calls another function; results flow through.
- Dataflow mode: shared upstream nodes run exactly once.
- Dataflow mode: unreachable nodes (not backward-reachable from `output`) never execute.
- Recursive function calls respect depth limits (delegate to cue later; for now, a mech-level cap).

**Implement:** `src/exec/function.rs`, `src/exec/workflow.rs`, `src/exec/dataflow.rs`.

**Acceptance:** Both modes execute the §12 example correctly.

---

### Deliverable 13 — Conversation management & history scoping

**Scope:** Implement the conversation model from §5: per-function conversation history accumulates across prompt blocks within the function; call blocks start fresh (transparent); compaction hooks (placeholder — actual compaction strategy can be a no-op initially, but the extension point must exist).

**Tests first:**
- Two sequential prompt blocks in one function share conversation history.
- A call block's callee sees empty history.
- Compaction hook is invoked at the configured threshold (count it, don't test strategy).
- History includes tool calls and tool results from the agent's internal loop.

**Implement:** `src/conversation.rs`; integrate into prompt block executor.

**Acceptance:** Conversation scoping matches §5.

---

### Deliverable 14 — Cue integration (`MechTask`)

**Scope:** Implement `MechTask` as a `cue::TaskNode`. A mech function invocation = one cue leaf task. Bridge mech's runtime errors to cue's `TaskOutcome`. Handle cue's model escalation by re-running with a higher-tier agent config (the function's agent cascade supplies the base; escalation bumps the model tier). Expose mech workflows to epic/other cue consumers.

**Tests first:**
- `MechTask` implements all 30 `TaskNode` methods; compile check.
- Running a mech workflow via `cue::Orchestrator<MechStore, _>` completes successfully for the §12 example.
- Task failure maps to the right `TaskOutcome` variant.
- Model escalation retries with a higher-tier model; final attempt's model matches expectation.
- State persistence: partially-executed workflow can resume (delegate to cue's resume machinery).

**Implement:** `src/cue_integration.rs`, `MechStore`.

**Acceptance:** Mech workflows run under cue orchestration.

---

### Deliverable 15 — CLI (`mech run`)

**Scope:** Minimal CLI: `mech run <workflow.yaml> --input <json>` loads a workflow, runs it standalone (without cue), prints the output as JSON. Useful for debugging without epic. Add `mech validate <workflow.yaml>` that runs load-time validation and exits non-zero on error.

**Tests first:**
- `mech validate` on the §12 example exits 0.
- `mech validate` on a broken workflow exits non-zero and prints all errors.
- `mech run` on a trivial workflow produces the expected JSON on stdout.
- CLI argument parsing: missing args, bad input JSON, missing file.

**Implement:** `src/bin/mech.rs`.

**Acceptance:** Developers can iterate on workflows without booting epic.

---

### Deliverable 16 — End-to-end integration test suite

**Scope:** Add `tests/` integration tests that run real workflows end-to-end: the §12 example, a recursive example, a dataflow example with shared nodes, an error-path example, a cue-orchestrated example. These use a deterministic fake LLM (canned responses) to keep tests hermetic and fast — no network, no real models.

**Tests first (this deliverable *is* tests):**
- Each scenario above, end-to-end, assertions on final output and intermediate state.
- Error scenarios assert the right error variant surfaces.
- Concurrent workflow invocations share `workflow.*` state safely.

**Implement:** `tests/fixtures/*.yaml`, `tests/common/fake_agent.rs`, `tests/*.rs`.

**Acceptance:** `cargo test -p mech` exercises every §5, §6, §9, §11 behavior through a real load→run→verify cycle.

---

### Deliverable 17 — Documentation polish & examples

**Scope:** Update `README.md` for the crate with a quickstart. Ensure the §12 worked example is copy-paste runnable. Add 2-3 small example workflows under `examples/` demonstrating imperative, dataflow, and recursive patterns. Update top-level `docs/STATUS.md` to mark mech complete.

**Tests first:**
- `cargo test --examples` compiles all examples.
- A doctest on the main `WorkflowLoader::load` shows minimal usage.

**Implement:** docs, examples.

**Acceptance:** A new user can go from zero to running a mech workflow using only the README + examples.

---

### Dependency Graph

```
1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9 → 10 → 11 → 12 → 13 → 14 → 15 → 16 → 17
                                  ↑        ↑
                                  └────────┘   (9 and 10 can overlap once 8 lands)
```

Deliverables 9 and 10 can be parallelized if two agents are available — they share only the `ExecutionContext` contract from deliverable 8. All other deliverables are strictly sequential.

### Exit Criteria for Mech v1

- All 17 deliverables complete with passing tests and clean reviews.
- `cargo test -p mech` green on Linux, macOS, Windows (CI).
- `cargo clippy -p mech --all-targets -- -D warnings` clean.
- The §12 worked example runs under both standalone `mech run` and cue-orchestrated modes.
- `docs/STATUS.md` mech section updated to "Complete".

---

## Appendix A: Research

Research notes from initial investigation (Perplexity, 2026-04). Raw Q&A format preserved for reference.

---

### A.1 Prior Art & Grammar Exploration

**Query:** Workflow DSL's for agent orchestration — CFG where each block is a prompt + JSON Schema output + CEL guard transitions. What exists? What could the grammar look like?

<img src="https://r2cdn.perplexity.ai/pplx-full-logo-primary-dark%402x.png" style="height:64px;margin-right:32px"/>

#### Prior Art & Close Relatives

**duckflux** is the closest known match to your design. It uses YAML, JSON Schema for typed outputs, and Google CEL for all conditions and transition expressions. Its author explicitly documents why CEL was chosen over JS eval (security surface), custom mini-DSLs (implementation burden), and JSONPath/JMESPath (poor logic support). The key difference: duckflux treats workflows as sequential pipelines with loops/conditionals, not general CFGs with arbitrary block transitions.[^1_1]

**LangGraph** is another strong reference point — it models workflows as explicit state graphs where nodes are LLM calls or tools, and edges are conditional transitions. The state is typed, but conditions are Python functions, not a sandboxed expression language.[^1_2]

**Azure Prompt Flow** offers DAG-based node graphs with typed inputs/outputs, visual editing, and is designed specifically for LLM orchestration. It skips the CFG model (no cycles) and uses Python for conditions.[^1_3]

**PayPal's declarative LLM DSL** (arxiv:2512.19769) is a CFG-style pipeline DSL deployed in production at scale, reporting 60% reduction in dev time vs. imperative code. The paper is worth reading as it targets the same pattern: prompt + schema + routing logic.[^1_4]

#### What Makes This Design Distinctive

The specific combination — **CFG (not just DAG) + structured output schema per block + CEL guard expressions** — hasn't been cleanly packaged as a standalone embeddable format. The CFG property (allowing cycles/backward edges) is what most tools deliberately exclude, since cycles in Airflow/Prefect/Argo are architecturally forbidden. duckflux adds cycles via `loop` constructs but doesn't expose raw CFG edge semantics.[^1_1]

#### Grammar Sketch (TOML)

```toml
# workflow.wfl

[workflow]
entry = "classify"

[block.classify]
prompt = """
You are a triage agent. Given the user request below, classify it.
Request: {{input.text}}
"""
schema = "schemas/classify.json"  # JSON Schema for output

[[block.classify.transitions]]
guard  = 'output.category == "billing"'
target = "billing_handler"

[[block.classify.transitions]]
guard  = 'output.category == "technical"'
target = "tech_handler"

[[block.classify.transitions]]
guard  = "true"   # fallback
target = "unknown_handler"

[block.billing_handler]
prompt = """
Resolve the following billing issue.
Category: {{output.category}}
Details: {{output.details}}
"""
schema = "schemas/resolution.json"

[[block.billing_handler.transitions]]
guard  = 'output.resolved == true'
target = "done"

[[block.billing_handler.transitions]]
guard  = 'output.resolved == false && context.attempts < 3'
target = "billing_handler"   # self-loop for retry

[[block.billing_handler.transitions]]
guard  = "true"
target = "escalate"

[block.done]
# terminal — no transitions

[block.escalate]
# terminal — no transitions
```

Design notes:

- **Guards are evaluated in order** — first truthy wins, so the fallback `"true"` belongs at the end.
- **`context`** is a mutable scratchpad for cross-block state like retry counts, distinct from `output` which is the structured LLM response for the current block.[^1_1]
- **Schemas are external JSON Schema files** or inline YAML objects.
- **Self-loops are first-class** (e.g., `billing_handler` → `billing_handler`) since this is a CFG, not a DAG.
- CEL fits perfectly: expressions like `output.resolved == true`, `context.attempts < 3`, or `"billing" in output.tags` are all valid CEL with no escape hatches.[^1_5][^1_1]

#### Alternative Syntax: Compact/Rust-Friendly

```
block classify {
  prompt: "You are a triage agent. Classify: {{input.text}}"
  schema: { type: "object", required: ["category", "confidence"] }

  -> billing_handler  when output.category == "billing"
  -> tech_handler     when output.category == "technical"
  -> unknown_handler  // implicit fallback
}

block billing_handler {
  prompt: "Resolve billing issue: {{output.details}}"
  schema: { type: "object", required: ["resolved", "notes"] }

  -> done           when output.resolved == true
  -> billing_handler when output.resolved == false && context.attempts < 3
  -> escalate
}
```

#### Rust Crates of Interest

- **`cel-interpreter`** or **`cel-rust`** — CEL evaluator crates
- **`jsonschema`** — for validating LLM outputs against per-block schemas
- **`serde_json` + `serde_yaml`** — for deserializing the workflow spec
- **`pest`** or **`nom`** — if a custom grammar parser is preferred over YAML

[^1_1]: https://ggondim.notion.site/duckflux-A-Declarative-Workflow-DSL-Born-from-the-Multi-Agent-Orchestration-Gap-31f9e3681d4880899242fb64ca78f882
[^1_2]: https://dev.to/rosidotidev/in-depth-comparison-workflow-control-with-langgraph-and-crewai-ae3
[^1_3]: https://learn.microsoft.com/en-us/azure/ai-foundry/concepts/prompt-flow
[^1_4]: https://arxiv.org/abs/2512.19769
[^1_5]: https://www.mintlify.com/agentgateway/agentgateway/concepts/cel-expressions

---

### A.2 YAML Grammar

**Query:** YAML is more readable/understood by users. Rewrite the grammar in YAML.

```yaml
workflow:
  entry: classify

blocks:
  classify:
    prompt: |
      You are a triage agent. Given the user request below, classify it.
      Request: {{input.text}}
    schema:
      type: object
      required: [category, confidence]
      properties:
        category:
          type: string
          enum: [billing, technical, general]
        confidence:
          type: number
    transitions:
      - when: 'output.category == "billing"'
        goto: billing_handler
      - when: 'output.category == "technical"'
        goto: tech_handler
      - goto: unknown_handler   # fallback — no 'when' means unconditional

  billing_handler:
    prompt: |
      Resolve the following billing issue.
      Details: {{output.details}}
    schema:
      type: object
      required: [resolved, notes]
      properties:
        resolved:
          type: boolean
        notes:
          type: string
    transitions:
      - when: 'output.resolved == true'
        goto: done
      - when: 'output.resolved == false && context.attempts < 3'
        goto: billing_handler   # self-loop / retry
      - goto: escalate

  done:
    prompt: |
      Summarize the resolution for the user.
      Notes: {{output.notes}}
    schema:
      type: object
      required: [summary]
      properties:
        summary:
          type: string
    # no transitions = terminal block

  escalate:
    prompt: |
      Draft an escalation notice for a human agent.
      History: {{context.history}}
    schema:
      type: object
      required: [notice]
      properties:
        notice:
          type: string
```

#### Key Design Decisions

**Inline vs. external schemas** — Inlining the JSON Schema directly as YAML keeps the workflow self-contained. For large, reused schemas, support a `$ref` string:

```yaml
schema: "./schemas/resolution.json"
```

The deserializer detects `string` vs. `object` for the `schema` field and loads accordingly.

**Fallback transition** — A transition entry with no `when` key is the unconditional fallback. Last-wins-by-position is predictable and matches how people read YAML lists top-to-bottom. Cleaner than a magic `when: "true"` CEL literal.

**Template variables** — Three scopes:

| Variable prefix | Meaning |
| :-- | :-- |
| `{{input.*}}` | The workflow's initial input payload |
| `{{output.*}}` | The structured JSON output of the *current* block |
| `{{context.*}}` | Mutable cross-block scratchpad (retry counts, accumulated history, etc.) |

**Terminal detection** — A block with no `transitions` key (or an empty list) is implicitly terminal.

#### Rust Deserialization Sketch

```rust
#[derive(Deserialize)]
struct Workflow {
    entry: String,
    blocks: HashMap<String, Block>,
}

#[derive(Deserialize)]
struct Block {
    prompt: String,
    schema: SchemaValue,          // String (path/$ref) or serde_json::Value (inline)
    #[serde(default)]
    transitions: Vec<Transition>,
}

#[derive(Deserialize)]
struct Transition {
    #[serde(rename = "when")]
    guard: Option<String>,        // None = unconditional fallback
    #[serde(rename = "goto")]
    target: String,
}
```

---

### A.3 CTFG & Dataflow Extensions

**Query:** Can CTFG and dataflow graphs be modeled similarly for parallel agent workflows?

#### CTFG (Fork/Join)

A CTFG adds **fork/join** semantics — a block can spawn multiple parallel branches, and execution resumes when a condition over those branches is met.

```yaml
workflow:
  entry: triage

blocks:
  triage:
    prompt: |
      Analyze this support ticket and extract key dimensions.
      Ticket: {{input.text}}
    schema:
      type: object
      required: [topic, severity, language]
      properties:
        topic:    { type: string }
        severity: { type: string, enum: [low, medium, high] }
        language: { type: string }
    transitions:
      - goto: parallel_analysis

  parallel_analysis:
    fork:
      branches: [sentiment_check, policy_lookup, translation]
      join:
        strategy: all          # all | any | n_of_m
        # n: 2                 # only needed for n_of_m
        goto: synthesize

  sentiment_check:
    prompt: |
      Rate the sentiment of this ticket.
      Text: {{input.text}}
    schema:
      type: object
      required: [score, label]
      properties:
        score: { type: number }
        label: { type: string, enum: [positive, neutral, negative] }

  policy_lookup:
    prompt: |
      Identify relevant policy sections for topic: {{blocks.triage.output.topic}}
    schema:
      type: object
      required: [policies]
      properties:
        policies:
          type: array
          items: { type: string }

  translation:
    prompt: |
      Translate the ticket to English if language is not "en".
      Language: {{blocks.triage.output.language}}
      Text: {{input.text}}
    schema:
      type: object
      required: [text, was_translated]
      properties:
        text:           { type: string }
        was_translated: { type: boolean }

  synthesize:
    prompt: |
      Synthesize a response using:
      Sentiment: {{blocks.sentiment_check.output.label}}
      Policies:  {{blocks.policy_lookup.output.policies}}
      Ticket:    {{blocks.translation.output.text}}
    schema:
      type: object
      required: [response]
      properties:
        response: { type: string }
    transitions:
      - goto: done
```

`{{blocks.<name>.output.*}}` — a new scope for accessing named upstream block outputs, essential in parallel graphs where `{{output.*}}` is ambiguous.

The fork block carries no prompt or schema — it's a pure **control node**.

| Strategy | Meaning |
| :-- | :-- |
| `all` | Wait for every branch to complete |
| `any` | Resume as soon as the first branch completes (cancel others) |
| `n_of_m` | Resume when `n` of the `m` branches complete |

#### Dataflow (Push/Eager)

Dataflow flips the perspective — edges are **data dependencies**, not control transitions. A block becomes ready when all its declared inputs are available.

```yaml
workflow:
  mode: dataflow
  entry: [extract_facts, extract_entities]

blocks:
  extract_facts:
    prompt: |
      Extract factual claims from: {{input.text}}
    schema:
      type: object
      required: [facts]
      properties:
        facts: { type: array, items: { type: string } }

  extract_entities:
    prompt: |
      Extract named entities from: {{input.text}}
    schema:
      type: object
      required: [entities]
      properties:
        entities: { type: array, items: { type: string } }

  cross_reference:
    depends_on: [extract_facts, extract_entities]
    prompt: |
      Cross-reference these facts with these entities.
      Facts:    {{blocks.extract_facts.output.facts}}
      Entities: {{blocks.extract_entities.output.entities}}
    schema:
      type: object
      required: [references]
      properties:
        references: { type: array, items: { type: string } }

  score_facts:
    depends_on: [extract_facts]
    prompt: |
      Score the credibility of each fact.
      Facts: {{blocks.extract_facts.output.facts}}
    schema:
      type: object
      required: [scores]
      properties:
        scores: { type: array, items: { type: number } }

  final_report:
    depends_on: [cross_reference, score_facts]
    prompt: |
      Write a final analysis report.
      Cross-references: {{blocks.cross_reference.output.references}}
      Credibility scores: {{blocks.score_facts.output.scores}}
    schema:
      type: object
      required: [report]
      properties:
        report: { type: string }
```

#### Unified Model

All three modes share the same block primitive — they differ only in edge expression:

| Model | Edge mechanism | Cycles | Parallelism |
| :-- | :-- | :-- | :-- |
| **CFG** | `transitions` + CEL guards | Yes | No |
| **CTFG** | `fork`/`join` control nodes | Yes | Explicit |
| **Dataflow** | `depends_on` + `mode: dataflow` | No | Implicit |

All three can coexist in the same runtime. A CFG block that forks into parallel branches and later a dataflow subgraph is a legitimate mixed-mode workflow. The executor handles three node kinds: **prompt nodes** (have `prompt` + `schema`), **fork nodes** (pure control), and **join nodes** (implicit — materialized at the `goto` target of a fork's join). The dataflow `depends_on` model is syntactic sugar that compiles into fork/join pairs at load time.

---

### A.4 Pull-Oriented Dataflow

**Query:** Specifying entry points for dataflow is counter-intuitive. Shouldn't you specify the output and pull?

Two evaluation strategies in dataflow theory:[^4_1]

- **Data-driven (eager/push):** nodes fire as soon as inputs are available — specify *sources*, data propagates forward.
- **Demand-driven (lazy/pull):** activation propagates *backward* from a requested output — specify the *sink*, graph resolves dependencies on demand.

For an LLM workflow harness, **pull semantics are more intuitive**. You declare what you want, not what to start from.

#### Pull-Oriented YAML

```yaml
workflow:
  mode: dataflow
  output: final_report       # the one thing you're asking for

blocks:
  final_report:
    prompt: |
      Write a final analysis report.
      Cross-references: {{blocks.cross_reference.output.references}}
      Credibility:      {{blocks.score_facts.output.scores}}
    schema:
      type: object
      required: [report]
      properties:
        report: { type: string }
    depends_on: [cross_reference, score_facts]

  cross_reference:
    prompt: |
      Cross-reference these facts with these entities.
      Facts:    {{blocks.extract_facts.output.facts}}
      Entities: {{blocks.extract_entities.output.entities}}
    schema:
      type: object
      required: [references]
      properties:
        references: { type: array, items: { type: string } }
    depends_on: [extract_facts, extract_entities]

  score_facts:
    prompt: |
      Score the credibility of each fact.
      Facts: {{blocks.extract_facts.output.facts}}
    schema:
      type: object
      required: [scores]
      properties:
        scores: { type: array, items: { type: number } }
    depends_on: [extract_facts]

  extract_facts:
    prompt: |
      Extract factual claims from: {{input.text}}
    schema:
      type: object
      required: [facts]
      properties:
        facts: { type: array, items: { type: string } }

  extract_entities:
    prompt: |
      Extract named entities from: {{input.text}}
    schema:
      type: object
      required: [entities]
      properties:
        entities: { type: array, items: { type: string } }
```

Key change: `workflow.output` names the sink, `entry` disappears. The executor resolves by walking `depends_on` backward from the sink.

#### Benefits of Pull Model

1. **Dead node elimination.** Blocks not reachable backward from `output` are never executed — like a compiler dropping unreachable code.

2. **Multiple outputs are natural:**
```yaml
workflow:
  mode: dataflow
  output: [final_report, score_facts]   # both are sinks; shared deps run once
```

Shared upstream nodes run exactly once.

#### Execution Model

```
Author declares:   workflow.output → "final_report"
Executor does:
  1. Walk depends_on backward → build DAG
  2. Topo-sort → find execution levels
  3. Execute level-by-level (within-level parallelism is future work; today blocks within a level run sequentially)
  4. Return the output of the declared sink(s)
```

Same model as Make, Dask, and Bazel.

[^4_1]: https://www.sciencedirect.com/topics/computer-science/data-flow-graph
