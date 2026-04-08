# Cue Graph/Workflow DSL Spec

> **Status:** Spec in progress. Not ready for implementation.

## 1. Overview

<!-- What this DSL is, what problem it solves, how it relates to cue's recursive task orchestration. -->

TODO

## 2. Design Goals

<!-- Guiding principles: declarative, YAML-native, embeddable, three execution modes in one grammar, etc. -->

TODO

## 3. Core Concepts

<!-- Definitions: workflow, block, prompt, schema, transition, guard, context, template variable. -->

TODO

## 4. Workflow Modes

### 4.1 CFG (Control Flow Graph)

<!-- Sequential with conditional branching and cycles. Blocks connected via `transitions` with CEL guards. -->

TODO

### 4.2 CTFG (Control Taskflow Graph)

<!-- Fork/join parallelism. `fork` control nodes with `branches` + `join` strategy (all/any/n_of_m). -->

TODO

### 4.3 Dataflow

<!-- Dependency-driven scheduling. `depends_on` edges, pull-oriented `output` sink declaration, dead node elimination. -->

TODO

### 4.4 Mixed-Mode

<!-- Whether/how CFG, CTFG, and dataflow can coexist in a single workflow. -->

TODO

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

## 9. Fork / Join Semantics

<!-- Fork node structure, join strategies table, cancellation on `any`, result collection. -->

TODO

## 10. Dataflow Execution Model

<!-- Pull vs. push, `workflow.output` sink(s), backward dependency walk, topo-sort, level-parallel scheduling, dead node elimination, multiple outputs. -->

TODO

## 11. Context & State

<!-- Mutable `context` scratchpad, cross-block state, retry counters. Lifecycle and persistence. -->

TODO

## 12. Validation & Error Handling

<!-- Load-time validation: cycle detection (dataflow), CEL compilation, schema resolution, unreachable blocks. Runtime: schema validation failures, guard evaluation errors, timeout. -->

TODO

## 13. Integration with Cue

<!-- How workflow execution maps to cue's Orchestrator, TaskNode, TaskStore. Whether a workflow is a single cue task or each block is a cue task. -->

TODO

## 14. YAML Reference Grammar

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
