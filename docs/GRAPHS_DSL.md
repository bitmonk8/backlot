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

### 4.6 Theoretical Basis

The CDFG model is well-established in compiler theory (Ferrante et al., 1987 — Program Dependence Graph; Click & Paleczny — Sea of Nodes). The "control dominates, data constrains" resolution used here matches the PDG model: control-dependence edges determine whether a node is reached, data-dependence edges constrain ordering within reachable regions.

## 5. Block Specification

<!-- Full schema for a block: prompt, schema (inline or $ref), transitions, fork, depends_on. Which fields are valid in which mode. -->

TODO

## 6. Transitions & Guards

<!-- Ordered evaluation, CEL expression language, unconditional fallback (no `when`), self-loops. -->

TODO

## 7. Template Variables & Scoping

<!-- `{{input.*}}`, `{{output.*}}`, `{{context.*}}`, `{{blocks.<name>.output.*}}`. Scoping rules per mode. -->

TODO

## 8. Schema Handling

<!-- JSON Schema per block. Inline YAML vs. external `$ref` path. Validation at load time vs. runtime. -->

TODO

## 9. Context & State

<!-- Mutable `context` scratchpad, cross-block state, retry counters. Lifecycle and persistence. -->

TODO

## 10. Validation & Error Handling

<!-- Load-time validation: cycle detection (dataflow), CEL compilation, schema resolution, unreachable blocks. Runtime: schema validation failures, guard evaluation errors, timeout. -->

TODO

## 11. Integration with Cue

<!-- How workflow execution maps to cue's Orchestrator, TaskNode, TaskStore. Whether a workflow is a single cue task or each block is a cue task. -->

TODO

## 12. YAML Reference Grammar

<!-- Complete annotated YAML schema for the workflow format. -->

TODO

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
