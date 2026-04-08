# Cue Graph/Workflow DSL Spec

> **Status:** Spec in progress. Not ready for implementation.

## 1. Overview

A YAML-based DSL for defining agent workflows as typed, statically-validated graphs. Each workflow file declares one or more **functions** — callable units whose bodies are hybrid control-flow/data-flow graphs (CDFGs). Blocks within a function are LLM prompt calls with JSON Schema typed inputs and outputs, connected by control edges (CEL-guarded transitions) and data edges (`depends_on`).

### Motivation

Rust is the right language for the backlot runtime — type safety, tooling, performance. But Rust is too general-purpose for rapid iteration on task logic. Each new task type requires Rust code changes, recompilation, and the full development cycle. Meanwhile, dynamic languages (Python, JS) offer fast iteration but lack the rigid type systems and validation that LLM orchestration requires — models benefit from structural constraints, not flexibility.

This DSL occupies the middle ground: **declarative structure with static typing, without requiring compilation.** Workflow authors define what each block does (prompt + schema), how blocks connect (transitions + dependencies), and what expressions govern routing (CEL) — all in YAML files that can be modified, reloaded, and tested without touching Rust.

The DSL replaces the need to implement task types as Rust code. The cue orchestrator executes DSL functions the same way it executes native tasks — the function is the unit of work, the CDFG is the implementation.

### Relationship to Cue

Cue provides generic recursive task orchestration (`TaskNode`, `TaskStore`, `Orchestrator`). The DSL adds a declarative layer: a DSL function can serve as a cue task's implementation. The orchestrator drives decomposition, retry, escalation; the DSL drives the internal logic of each task.

## 2. Design Goals

1. **Single unified graph model.** No mode selection. Control edges and data edges coexist freely in one graph. The executor infers behavior from edge types present on each block.
2. **Functions as the callable unit.** A workflow file defines named functions. Functions call other functions. Parallelism is expressed at the function-call level (fork/join), not as a graph-level mode.
3. **Static typing via JSON Schema.** Every block declares its output schema. Type mismatches between a block's output and a downstream block's template references are caught at load time, not runtime.
4. **CEL for all expressions.** Transition guards, template expressions, and any computed values use CEL. No embedded Python, no custom expression language, no eval.
5. **YAML surface syntax.** Human-readable, LLM-readable, tooling-friendly. No custom parser required for the outer structure.
6. **Declarative, not imperative.** Workflows describe structure and constraints. The executor decides scheduling, parallelism within dataflow regions, and retry mechanics.
7. **Embeddable in cue.** A DSL function maps to a cue `TaskNode` implementation. The DSL does not replace cue's orchestration protocol — it provides a declarative way to define what a task does internally.

## 3. Core Concepts

- **Workflow file** — A YAML file declaring one or more functions.
- **Function** — A named callable unit. Its body is a CDFG (control-data flow graph) of blocks. Functions can call other functions.
- **Block** — A node in the graph. Prompt blocks invoke an LLM with a prompt template and validate the output against a JSON Schema. Call blocks invoke another function (with optional fork/join for parallelism).
- **Control edge** — A `transition` from one block to another, optionally guarded by a CEL expression (`when`). Evaluated in declaration order; first match wins. Supports cycles (self-loops, backward edges).
- **Data edge** — A `depends_on` declaration. The block cannot execute until all named dependencies have produced output. Acyclic by definition.
- **Activation rule** — A block with inbound control edges is *activated* when a transition targets it. A block with only data edges is activated implicitly when its dependencies are met. A block with both requires the transition to fire AND all dependencies to be satisfied. (Control gates activation; data gates readiness.)
- **Schema** — JSON Schema (inline YAML or `$ref` path) declaring the typed output of a block. Used for load-time validation of downstream template references.
- **Template variable** — Mustache-style references (`{{input.*}}`, `{{output.*}}`, `{{context.*}}`, `{{blocks.<name>.output.*}}`) interpolated into prompt text. Scoping rules defined in §7.
- **Guard** — A CEL expression on a transition. Evaluated against the current block's output and the workflow context.
- **Context** — A mutable key-value scratchpad scoped to a function invocation. Used for cross-block state (retry counters, accumulated data).

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
4. **Level-parallel scheduling.** Blocks within the same level (no mutual dependencies) execute concurrently. The executor advances level-by-level.
5. **Multiple sinks.** If multiple terminal blocks exist in a dataflow region, shared upstream blocks execute exactly once.

### 4.4 Function Calls

A **call block** invokes one or more functions. `call` accepts a single function name (string) or a list. Execution is **sequential by default**. The optional `parallel` property opts into concurrent execution and specifies the join strategy.

```yaml
# Sequential, single function
lookup:
  call: sentiment_check
  input: { text: "{{input.text}}" }

# Sequential, multiple functions — executed in list order
pipeline:
  call: [extract, validate, transform]
  input: { text: "{{input.text}}" }

# Parallel, multiple functions
analyze:
  call: [sentiment_check, policy_lookup, translation]
  parallel: all       # all | any | n_of_m
  input: { text: "{{input.text}}" }
```

**Sequential list execution:** Functions execute in list order. All receive the same `input`. Each function's output is accessible by name via `{{blocks.<name>.output.*}}` in subsequent blocks. The call block's own `output` is the output of the last function in the list.

**Parallel execution:** Functions execute concurrently as independent CDFGs. Results are collected per the join strategy:

| Strategy | Behavior |
|---|---|
| `all` | Wait for every function to complete. |
| `any` | Resume when the first function completes. Others are cancelled. |
| `n_of_m` | Resume when `n` functions complete (requires `n:` field). Others are cancelled. |

**Cancellation:** When `any` or `n_of_m` triggers early completion, remaining in-flight functions receive a cancellation signal. A cancelled function's output is not available — template references to cancelled functions are a runtime error. Callers using `any` or `n_of_m` should only reference outputs conditionally or use the join result which identifies which functions completed.

**Result collection:** All completed function outputs are accessible via `{{blocks.<name>.output.*}}` regardless of execution mode. For `any`, only the winning function's output is populated. For `n_of_m`, outputs of the `n` completed functions are populated; the rest are absent.

### 4.5 Function Definitions

A function declares its **input schema** (typed arguments) and zero or more **terminal blocks**.

```yaml
functions:
  sentiment_check:
    input:
      type: object
      required: [text]
      properties:
        text: { type: string }
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

**Terminal blocks** determine the function's return value:

- If `terminals` is specified: those blocks are terminal. Validated at load time (must exist, must have no outgoing transitions or data edges).
- If `terminals` is omitted: terminal blocks are inferred — any block with no outgoing control edges and no outgoing data edges.
- **Single terminal reached:** the function's output is that block's output.
- **Multiple terminals (CFG paths):** the function's output is the output of whichever terminal was reached during execution.
- **Multiple terminals (dataflow sinks):** all terminal outputs are collected into a map keyed by block name.

### 4.6 Conversation Model

Each function invocation creates a new **conversation** — an ordered message list (system prompt + alternating user/assistant pairs) that is passed to the LLM on each block execution within that function. Conversation history follows **control edges only**. Data edges carry structured output, never conversation history.

**Core rules:**

1. **Function = conversation boundary.** A function invocation creates a fresh, empty conversation. When the function returns, its conversation is discarded. The caller sees only the function's structured output — analogous to a stack frame that is popped on return.
2. **Control edges carry history forward.** When a transition fires from block A to block B, block B's LLM call includes the full conversation accumulated along the control-flow path that reached it. Each prompt block appends a user message (the rendered prompt) and an assistant message (the LLM's structured response) to the conversation.
3. **Data edges do not carry history.** A block activated by `depends_on` receives its dependencies' structured outputs via template variables (`{{blocks.<name>.output.*}}`), but does not inherit their conversation history. Dataflow blocks are single-turn by nature.
4. **Call blocks reset conversation.** A `call` block invokes a sub-function, which gets its own conversation. The sub-function's internal conversation is invisible to the caller. The caller's conversation is not affected — call blocks are transparent (they produce structured output but add no messages to the parent conversation).
5. **Parallel branches are conversation-isolated.** Parallel function calls (via `parallel: all|any|n_of_m`) each get independent conversations. No merge problem exists because there is no shared history to merge.

**Cycles and history accumulation:**

Self-loops and backward transitions accumulate conversation history. A block that transitions back to itself (retry pattern) sees its prior prompt+response pairs on each iteration. This is intentional — the LLM benefits from seeing its prior attempts. Workflow authors should use `LimitsConfig` (retry budgets) or CEL guards (e.g., `context.attempts < 3`) to bound cycles and prevent unbounded history growth.

**Implications for mixed CDFG graphs:**

In a function with both control edges and data edges, the conversation follows the control-flow spine. Dataflow blocks that execute in parallel within a level are single-turn — they receive structured data from their dependencies but no conversational context. This avoids nondeterministic message interleaving from parallel execution.

A hybrid block (inbound control edge + inbound data edges, per §4.3) inherits conversation from the control edge that activated it. Its data dependencies contribute structured output only.

**System prompts are layered: workflow default + function override.** The workflow file may declare a default `system` field. Each function may override it with its own `system` field. The resolved system prompt becomes the system message for the function's conversation. System prompts support template variables (`{{input.*}}`, `{{context.*}}`), allowing the caller to parameterize persona.

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

If neither the function nor the workflow declares a `system` field, the conversation has no system message. Per-block system prompt variation is not supported — per-block instructions belong in the block's `prompt` template. If a block needs a fundamentally different persona, extract it to a separate function.

**History compaction.** Long-running functions (especially those with cycles) accumulate conversation history that may exceed the model's context window. The DSL provides a token-budget compaction mechanism, modeled after Pi Agent's approach.

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

**Custom compaction functions.** The `compaction` field accepts an optional `fn` property naming a DSL function (or registered Rust handler) that replaces the built-in summarizer. The custom function receives the messages to summarize as input and returns a summary string. This allows domain-specific compaction logic (e.g., preserving specific structured data, using a cheaper model for summarization, or applying non-LLM compression).

```yaml
compaction:
  keep_recent_tokens: 20000
  reserve_tokens: 16384
  fn: custom_summarizer         # a DSL function or registered handler
```

**Model selection is per-block.** Each prompt block may specify a `model` field overriding the function or workflow default. Different blocks within the same conversation can use different models — the executor switches models between turns while preserving the shared conversation history. This is supported by flick's architecture: `Context` (message history) is model-agnostic, and model resolution is per-`FlickClient` instance. The executor constructs a new client when the model changes between blocks.

```yaml
blocks:
  draft:
    model: haiku
    prompt: "Write a first draft of {{input.topic}}"
    schema: { ... }
    transitions:
      - goto: critique

  critique:
    model: opus
    prompt: "Critique the draft and identify weaknesses."
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
| `prompt` | string | Yes | Template string rendered with variable substitution (§7). Becomes the user message in the conversation. |
| `schema` | object \| string | Yes | JSON Schema for the LLM's structured output. Inline YAML object or `$ref` string path (§8). |
| `model` | string | No | Model override for this block. Resolves via flick's `ModelRegistry`. If omitted, inherits from function or workflow default. |
| `transitions` | list | No | Outbound control edges. Each entry has `goto` (required) and `when` (optional CEL guard). See §6. |
| `depends_on` | list of strings | No | Block names whose outputs must be available before this block executes. Acyclic (enforced at load time). |
| `set_context` | object | No | CEL expressions evaluated against the block's output, writing results to the function's mutable context. Keys are context field names, values are CEL expressions. See §9. |

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
  model: sonnet
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
    attempt_count: "has(context.attempt_count) ? context.attempt_count + 1 : 1"
  transitions:
    - when: 'output.severity >= 4'
      goto: escalate
    - goto: respond
```

### 5.2 Call Block

Invokes one or more named functions. The called function(s) execute with their own conversation and return structured output. Call blocks are transparent to the parent conversation — they produce output but add no messages to the caller's history.

| Field | Type | Required | Description |
|---|---|---|---|
| `call` | string \| list of strings | Yes | Function name(s) to invoke. Single string for one function, list for sequential or parallel execution. |
| `input` | object | Yes | Input mapping. Keys are the called function's input field names, values are CEL expressions or template strings resolved in the caller's scope. |
| `parallel` | string | No | Join strategy for list calls: `all`, `any`, `n_of_m`. If omitted, list calls execute sequentially. Ignored for single-function calls. |
| `n` | integer | No | Required when `parallel: n_of_m`. Number of completions needed before resuming. |
| `transitions` | list | No | Outbound control edges (same as prompt blocks). |
| `depends_on` | list of strings | No | Block names whose outputs must be available before this block executes (same as prompt blocks). |
| `set_context` | object | No | CEL expressions against the call block's output, writing to context (same as prompt blocks). |

**Call block output:** For a single function call, the block's output is the function's return value. For sequential list calls, the output is the last function's return value (all are accessible via `{{blocks.<fn_name>.output.*}}`). For parallel calls, see §4.4 result collection rules.

**Minimal call block:**

```yaml
lookup:
  call: sentiment_check
  input: { text: "{{input.text}}" }
```

**Parallel call block:**

```yaml
analyze:
  call: [sentiment_check, policy_lookup, translation]
  parallel: all
  input: { text: "{{input.text}}" }
  transitions:
    - goto: synthesize
```

### 5.3 Field Validity Rules

| Field | Prompt block | Call block |
|---|---|---|
| `prompt` | Required | Forbidden |
| `schema` | Required | Forbidden |
| `model` | Optional | Forbidden |
| `call` | Forbidden | Required |
| `input` | Forbidden | Required |
| `parallel` | Forbidden | Optional |
| `n` | Forbidden | Optional (requires `parallel: n_of_m`) |
| `transitions` | Optional | Optional |
| `depends_on` | Optional | Optional |
| `set_context` | Optional | Optional |

**Load-time enforcement:** A block with both `prompt` and `call` is an error. A block with neither is an error. A block with `schema` or `model` but no `prompt` is an error. A block with `parallel` or `n` but no `call` is an error. A block with `n` but `parallel` not set to `n_of_m` is an error.

### 5.4 Block Identity and Naming

Block names are the YAML keys under the `blocks:` map. Names must be valid identifiers: `[a-z][a-z0-9_]*` (lowercase, underscore-separated). Names must be unique within a function. Reserved names: `input`, `output`, `context` (these conflict with template variable namespaces).

### 5.5 Default Model Resolution

Model resolution follows a three-level cascade:

1. **Block-level** `model` field (highest priority)
2. **Function-level** `model` field (declared alongside `input`, `system`, etc.)
3. **Workflow-level** `model` field (declared at the top level)

If no model is specified at any level, the executor uses the runtime's default model. All model names resolve via flick's `ModelRegistry` at load time — an unresolvable model name is a load-time error.

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

All guard expressions use [CEL (Common Expression Language)](https://cel.dev/). CEL is sandboxed, side-effect-free, and evaluates in constant time (no loops, no I/O).

**Available variables in guard expressions:**

| Variable | Type | Description |
|---|---|---|
| `output` | object | The current block's structured output (validated against its schema). |
| `input` | object | The function's input arguments. |
| `context` | object | The function's mutable context scratchpad. |

**Not available:** `blocks.*` — upstream block outputs are not directly accessible in guards. If a guard needs upstream data, the workflow author should pipe it through `set_context` on an earlier block and reference `context.*` in the guard.

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
    draft_attempts: "has(context.draft_attempts) ? context.draft_attempts + 1 : 1"
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

## 7. Template Variables & Scoping

Template variables are `{{...}}` references interpolated into prompt text, `input` mappings on call blocks, and `system` prompt strings. The expression inside the braces is a dotted path into a named namespace.

### 7.1 Namespaces

| Namespace | Syntax | Description |
|---|---|---|
| `input` | `{{input.field}}` | The function's input arguments (immutable for the function's lifetime). |
| `output` | `{{output.field}}` | The current block's own output. Only valid in `set_context` and `transitions` (the block must have already produced output). |
| `context` | `{{context.field}}` | The function's mutable context scratchpad (§9). |
| `blocks` | `{{blocks.<name>.output.field}}` | A named block's output. The referenced block must have produced output (enforced by data edges or control-flow ordering). |

### 7.2 Resolution Rules

Template variables are resolved **at render time** — just before the block executes (for `prompt` and `input` fields) or just after (for `set_context` and `transitions`).

**Resolution order for a prompt block:**

1. Resolve `{{input.*}}` and `{{context.*}}` and `{{blocks.*.output.*}}` in the `prompt` template.
2. Send the rendered prompt to the LLM. Receive structured output.
3. Validate output against `schema`.
4. Resolve `{{output.*}}` in `set_context` expressions (CEL, not template syntax — `output.field` not `{{output.field}}`).
5. Resolve `{{output.*}}`, `{{input.*}}`, `{{context.*}}` in `transitions` guard expressions (CEL).

**Resolution order for a call block:**

1. Resolve all template variables in `input` field values.
2. Invoke the called function(s) with resolved input.
3. Collect output(s).
4. Resolve `set_context` and `transitions` against the call block's output.

### 7.3 Availability by Block Position

Which namespaces are available depends on how the block was activated:

| Block activation | `input` | `context` | `blocks.*` | `output` |
|---|---|---|---|---|
| Entry point (no inbound edges) | Yes | Yes (initial state) | No (no predecessors) | After execution only |
| Control-flow target (via transition) | Yes | Yes | Only blocks on the control path that have executed | After execution only |
| Dataflow node (via `depends_on`) | Yes | Yes | Only declared dependencies | After execution only |
| Hybrid (control + data) | Yes | Yes | Dependencies + control-path predecessors | After execution only |

**Key constraint:** `{{blocks.<name>.output.*}}` references must be statically resolvable — the named block must be guaranteed to have executed before the referencing block. This is enforced at load time:

- A block referencing `{{blocks.foo.output.*}}` must have `foo` in its `depends_on` list, OR `foo` must **dominate** the block in the control-flow graph (every control-flow path to the block passes through `foo`).
- If neither condition is met: load-time error.

### 7.4 Nested Field Access

Template variables support dotted paths for nested object access:

```yaml
prompt: |
  The user's name is {{input.user.name}}.
  Their top preference is {{blocks.preferences.output.items[0].label}}.
```

Array indexing uses bracket notation: `field[0]`, `field[1]`. Out-of-bounds access is a runtime error.

**Optional field access:** Use CEL's `has()` function in guards and `set_context`, but template variables in `prompt` strings do not support conditional logic. If a field may be absent, the workflow author should either:

1. Ensure the schema marks it `required`, or
2. Pipe it through `set_context` with a CEL default value, then reference `{{context.field}}` in the prompt.

```yaml
# Safely defaulting an optional field via set_context
check:
  prompt: "Check status"
  schema:
    type: object
    properties:
      note: { type: string }      # not required — may be absent
  set_context:
    safe_note: 'has(output.note) ? output.note : "No note provided"'
  transitions:
    - goto: report

report:
  prompt: "Report: {{context.safe_note}}"
  schema: { ... }
```

### 7.5 Template Syntax Details

- **Delimiters:** `{{` and `}}`. Literal braces in prompt text must be escaped as `{{"{"}}` and `{{"}"}}` (CEL string expression).
- **Whitespace:** `{{ input.text }}` is equivalent to `{{input.text}}` — leading/trailing whitespace inside delimiters is trimmed.
- **Rendering:** Template variable values are serialized to their JSON string representation when interpolated into prompt text. Objects and arrays are rendered as compact JSON. Strings are rendered without quotes. Numbers and booleans render as their literal representation.
- **Undefined variable:** A template reference to a namespace or field that doesn't exist is a **runtime error**. The block does not execute. The executor reports which variable failed resolution and in which block.

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

**Template type checking** is best-effort static analysis. The loader traces dotted paths (`output.field.subfield`) through the schema's `properties` tree and verifies the field exists. Type mismatches (e.g., referencing `output.count` as a string when the schema declares it as `integer`) produce warnings, not errors — the LLM may return compatible values that don't match the JSON Schema type precisely.

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

Each function invocation has a **context** — a mutable key-value map that persists across block executions within that invocation. Context provides cross-block state that is not part of the conversation history or structured output chain.

### 9.1 Lifecycle

1. **Creation.** When a function is invoked, its context is initialized as an empty map `{}`.
2. **Reading.** Any block can read context via `{{context.field}}` in templates or `context.field` in CEL expressions (guards, `set_context`).
3. **Writing.** Blocks write to context via the `set_context` field (§5). Writes happen after the block produces output, before transitions are evaluated.
4. **Destruction.** When the function returns, its context is discarded. The caller cannot access the callee's context — only its structured output.

Context does not cross function boundaries. A called function starts with a fresh context. This mirrors the conversation model (§4.6) — function = stack frame.

### 9.2 Writing Context: `set_context`

The `set_context` field is a map of context keys to CEL expressions:

```yaml
set_context:
  attempt_count: "has(context.attempt_count) ? context.attempt_count + 1 : 1"
  last_score: "output.score"
  best_result: "!has(context.best_result) || output.score > context.best_result.score ? output : context.best_result"
```

**Evaluation rules:**

1. All `set_context` expressions are evaluated after the block produces output.
2. Expressions have access to `output`, `input`, `context`, and `blocks.*` (same scope as the block's templates).
3. Expressions are evaluated **atomically** — all expressions see the context state from *before* `set_context` runs, not partially-updated state. This prevents order-dependent behavior within a single `set_context` block.
4. After all expressions are evaluated, results are merged into the context map (new keys are added, existing keys are overwritten).

### 9.3 Context in Cycles

Context is the primary mechanism for bounding and controlling cycles. Self-loops and backward transitions accumulate both conversation history and context state.

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
    attempts: "has(context.attempts) ? context.attempts + 1 : 1"
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
  all_results: "has(context.all_results) ? context.all_results + [output] : [output]"
```

### 9.4 Context and Compaction

When conversation compaction fires (§4.6), context is **not** compacted — it retains its full state. Context is separate from conversation history. The compaction summary may reference context values (e.g., "after 3 attempts, best score was 0.87"), but the context map itself is preserved verbatim.

This means context is suitable for data that must survive compaction: running totals, best-so-far results, iteration counters. Conversation history degrades gracefully through summarization; context does not degrade.

### 9.5 Context and Parallel Execution

Parallel function calls (via `parallel: all|any|n_of_m`) each get their own context (they're separate function invocations). The parent block's `set_context` runs after the parallel calls complete, in the parent function's context scope.

Dataflow blocks within a single function that execute in parallel at the same topological level share the function's context. However, since `set_context` writes happen after block execution, and parallel blocks cannot have data dependencies on each other, there is no write conflict — each block's `set_context` writes are applied in an arbitrary order after all blocks at that level complete. If two parallel blocks write the same context key, the result is nondeterministic. Load-time validation flags this as a **warning**.

### 9.6 Initial Context

A function's `input` field defines the function's typed arguments, not its context. To seed context from input:

```yaml
functions:
  process:
    input:
      type: object
      required: [text]
      properties:
        text: { type: string }
    initial_context:
      language: '"en"'                           # CEL literal
      source_length: "size(input.text)"          # CEL expression over input
    blocks: { ... }
```

The optional `initial_context` field on a function is a map of context keys to CEL expressions evaluated against the function's input at invocation time. If omitted, the initial context is empty.

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

#### Graph Checks

| Check | Severity | Description |
|---|---|---|
| Dataflow cycle detection | Error | `depends_on` edges must form a DAG. Cycles in data edges are forbidden. |
| Transition target existence | Error | Every `goto` target must name a block in the same function. |
| Call target existence | Error | Every `call` target must name a function in the workflow (or a registered external function). |
| Unreachable blocks | Warning | Blocks with no inbound edges (control or data) and that are not entry points are never executed. |
| Dead transitions | Warning | Transitions after an unconditional fallback are unreachable. |
| Parallel context write conflict | Warning | Two blocks that may execute in parallel both write the same `set_context` key. |

#### Type Checks

| Check | Severity | Description |
|---|---|---|
| Schema validity | Error | All schemas parse as valid JSON Schema. Root type is `object`. `required` is non-empty. |
| External schema resolution | Error | All `$ref:` paths resolve to existing files. Workflow-level `$ref:#name` references exist in `schemas` map. |
| Template reference resolution | Error | `{{blocks.<name>.output.field}}` — the named block exists, the field exists in its schema. |
| Template reference reachability | Error | The referenced block is guaranteed to have executed before the referencing block (domination or `depends_on`). |
| CEL compilation | Error | All `when` guards and `set_context` expressions compile as valid CEL. |
| CEL variable scope | Error | Guard expressions only reference `output`, `input`, `context`. Template variables only reference valid namespaces. |
| Model resolution | Error | All `model` fields (block, function, workflow) resolve via flick's `ModelRegistry`. |
| Input schema match | Error | Call block `input` fields must provide all `required` fields declared in the called function's `input` schema. |

### 10.2 Runtime Errors

Runtime errors occur during block execution. The error handling strategy depends on the error type.

#### Schema Validation Failure

The LLM returned output that does not validate against the block's JSON Schema.

- **Behavior:** The block fails. Error message includes the block name, the schema violation details (field, expected type, actual value), and the raw LLM output.
- **Propagation:** The block is marked failed. If the block is on a control-flow path, the function fails. If cue orchestration wraps this function, cue's retry/escalation handles the failure (§11).

#### Guard Evaluation Error

A CEL guard expression raises a runtime error (e.g., type mismatch, accessing a field on null).

- **Behavior:** The transition is treated as non-matching (guard is false). Evaluation continues to the next transition. If no transitions match, the block becomes a de facto terminal (§6.5).
- **Reporting:** A warning is emitted with the block name, the guard expression, and the CEL error. This is not a fatal error — the workflow continues.

**Rationale:** Treating guard errors as false (skip) rather than fatal keeps the workflow running. The unconditional fallback (if present) catches the case. If no fallback exists, the block terminates the path — which is visible in the function's output.

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

The DSL does not replace cue — it provides a declarative way to define what a cue task does internally. A DSL function is an **implementation strategy** for a `TaskNode`, not a replacement for the orchestration protocol.

### 11.1 Mapping: Function = Leaf Task Implementation

A cue leaf task can be implemented by a DSL function instead of a direct agent call. When the orchestrator calls `execute_leaf()` on such a task, the task's implementation loads and executes a DSL function, returning the function's structured output as the leaf result.

```
Cue Orchestrator
  └── execute_task(root)
        └── assess → Branch
              └── decompose → [subtask_1, subtask_2, subtask_3]
                    ├── execute_task(subtask_1)
                    │     └── assess → Leaf
                    │           └── execute_leaf()
                    │                 └── DSL function: "triage_workflow"
                    │                       ├── block: classify
                    │                       ├── block: analyze
                    │                       └── block: respond  ← output
                    ├── execute_task(subtask_2) ...
                    └── execute_task(subtask_3) ...
```

**The DSL function is opaque to cue.** Cue sees a leaf task that produces a `TaskOutcome`. The function's internal blocks, transitions, context, and conversation are invisible to the orchestrator. The function either succeeds (returning structured output) or fails (returning a failure reason).

### 11.2 Mapping: Each Block is NOT a Cue Task

Individual blocks are not modeled as cue tasks. This is deliberate:

- **Granularity mismatch.** Cue tasks have assessment, decomposition, verification, fix loops, recovery. Blocks are simpler — prompt → output → transition. Wrapping each block as a cue task would impose unnecessary overhead and state machine complexity.
- **Conversation continuity.** Blocks within a function share a conversation (§4.6). Cue tasks are independent — they don't share conversation state. Making each block a separate task would break the conversation model.
- **Retry semantics differ.** Cue's retry escalates models (Haiku → Sonnet → Opus). Block-level retry is a self-loop with accumulated context. These are different mechanisms for different purposes.

**The DSL executor is a separate execution engine** that runs inside `execute_leaf()`. It handles block scheduling, template resolution, LLM calls, conversation management, and context mutation. Cue handles the outer loop: assessment, decomposition, verification, fix loops, recovery.

### 11.3 DSL-Backed TaskNode

A `TaskNode` implementation that executes DSL functions needs:

```
DslTask {
    // Standard cue fields
    id: TaskId,
    phase: TaskPhase,
    goal: String,
    ...

    // DSL-specific
    function_name: String,          // which DSL function to execute
    workflow: Arc<LoadedWorkflow>,   // parsed + validated workflow (shared across tasks)
    input: serde_json::Value,       // input arguments for the function
}
```

**`execute_leaf` implementation:**

1. Look up the function by name in the loaded workflow.
2. Create a new `FunctionExecution` (conversation, context, block states).
3. Run the DSL executor (§4.3 activation rules, §4.6 conversation model).
4. On success: return `TaskOutcome::Success`. The function's output is stored for the parent to access.
5. On failure (any block fails, template error, schema error): return `TaskOutcome::Failed { reason }`.

**`assess` implementation:** DSL-backed tasks are always leaves. The `assess` method returns `AssessmentResult { path: Leaf, model: <workflow default>, ... }`.

**`verify_branch` / `decompose`:** Not applicable — DSL tasks are leaves. These methods should not be called on a DSL task. If they are (implementation error), they return an error.

### 11.4 Cue Retry and DSL Failures

When a DSL function fails (e.g., schema validation failure on a block), cue's outer loop handles retry:

1. `execute_leaf()` returns `TaskOutcome::Failed { reason: "Block 'analyze' schema validation failed: ..." }`.
2. Cue's retry mechanism re-calls `execute_leaf()` — possibly with an escalated model.
3. The DSL function re-executes from scratch (fresh conversation, fresh context). Model escalation from cue overrides the workflow-level default model, but block-level `model` overrides still take precedence.

**Model escalation interaction:**

| Level | Source | Overridden by cue escalation? |
|---|---|---|
| Workflow default | `workflow.model` | Yes |
| Function default | `function.model` | Yes |
| Block override | `block.model` | No — block-level is intentional (e.g., cheap model for triage, expensive model for synthesis) |

### 11.5 Workflow as Branch Task

An alternative mapping: a cue branch task uses a DSL function for its **decomposition** — the function's blocks define the subtask structure rather than executing directly.

This is a future extension. The current design maps DSL functions to leaf execution only. Branch decomposition remains the domain of agent calls (via `TaskNode::decompose`). If this extension proves valuable, a DSL function's terminal blocks would produce `SubtaskSpec` values instead of free-form output.

### 11.6 Workflow Loading and Sharing

Workflows are loaded once and shared across tasks:

1. **Load phase:** Parse YAML, validate (§10.1), compile CEL expressions, resolve schemas. Produce a `LoadedWorkflow` (immutable, `Arc`-wrapped).
2. **Execution phase:** Each `execute_leaf()` call creates a fresh `FunctionExecution` referencing the shared `LoadedWorkflow`. No re-parsing or re-validation.
3. **Registration:** Workflows are registered in the `TaskStore` (or a workflow registry accessible to it) so that DSL-backed tasks can reference them by name.

### 11.7 Event Integration

DSL block execution emits events that map to the application's event system:

| DSL event | Description |
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
  model: <string>                               # optional — default model name (flick ModelRegistry)
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

    system: <string>                            # optional — override workflow system prompt
    model: <string>                             # optional — override workflow default model
    terminals: [<block_name>, ...]              # optional — explicit terminal blocks (auto-detected if omitted)

    initial_context:                            # optional — seed context from input
      <key>: <cel_expression>                   #   evaluated against input at invocation

    compaction:                                 # optional — override workflow compaction config
      keep_recent_tokens: <integer>
      reserve_tokens: <integer>
      fn: <string>                              # optional

    # ─── Blocks ────────────────────────────────────────────

    blocks:
      # ── Prompt Block ──
      <block_name>:                             # identifier: [a-z][a-z0-9_]*, not input/output/context
        prompt: <string>                        # required — template string ({{...}} variables)
        schema: <object | "$ref:path" | "$ref:#name">  # required — output JSON Schema
        model: <string>                         # optional — override function/workflow model

        depends_on: [<block_name>, ...]         # optional — data edges (must be acyclic)

        set_context:                            # optional — write to function context after execution
          <key>: <cel_expression>               #   evaluated against output, input, context, blocks.*

        transitions:                            # optional — outbound control edges
          - when: <cel_expression>              # optional — CEL guard (omit for unconditional)
            goto: <block_name>                  # required — target block in same function

      # ── Call Block ──
      <block_name>:
        call: <string | [string, ...]>          # required — function name(s)
        input:                                  # required — input mapping for called function(s)
          <field>: <template_or_cel_expr>

        parallel: <all | any | n_of_m>          # optional — join strategy (list calls only)
        n: <integer>                            # optional — required when parallel: n_of_m

        depends_on: [<block_name>, ...]         # optional
        set_context:                            # optional
          <key>: <cel_expression>
        transitions:                            # optional
          - when: <cel_expression>              # optional
            goto: <block_name>                  # required
```

### 12.1 Field Type Summary

| Field | Type | Where |
|---|---|---|
| `workflow.system` | Template string | Top-level |
| `workflow.model` | Model name string | Top-level |
| `workflow.schemas` | Map of name → JSON Schema object | Top-level |
| `workflow.compaction` | Compaction config object | Top-level |
| `function.input` | JSON Schema (root type: object) | Function |
| `function.system` | Template string | Function |
| `function.model` | Model name string | Function |
| `function.terminals` | List of block name strings | Function |
| `function.initial_context` | Map of key → CEL expression | Function |
| `function.compaction` | Compaction config object | Function |
| `block.prompt` | Template string | Prompt block |
| `block.schema` | JSON Schema object or `$ref` string | Prompt block |
| `block.model` | Model name string | Prompt block |
| `block.call` | String or list of strings | Call block |
| `block.input` | Map of field → template/CEL expression | Call block |
| `block.parallel` | Enum: `all`, `any`, `n_of_m` | Call block |
| `block.n` | Positive integer | Call block |
| `block.depends_on` | List of block name strings | Any block |
| `block.set_context` | Map of key → CEL expression | Any block |
| `block.transitions` | List of transition entries | Any block |
| `transition.when` | CEL expression string | Transition |
| `transition.goto` | Block name string | Transition |

### 12.2 Complete Example

```yaml
workflow:
  system: "You are a customer support agent."
  model: sonnet
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

    initial_context:
      attempts: '0'

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
        model: opus
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
  3. Execute level-by-level, parallelising within each level
  4. Return the output of the declared sink(s)
```

Same model as Make, Dask, and Bazel.

[^4_1]: https://www.sciencedirect.com/topics/computer-science/data-flow-graph
