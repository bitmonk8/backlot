<img src="https://r2cdn.perplexity.ai/pplx-full-logo-primary-dark%402x.png" style="height:64px;margin-right:32px"/>

# I am researching workflow DSL’s for my agent orchestration harness.

The general idea is that a workflow is a CFG where each block is a:
Prompt, a JSON Schema for the output, a set of CEL expressions with each a block transition to be taken if the expression evaluates to true.

Has something like that been made? Anything close? What could an intuitive grammar for such a DSL look like?

This is a rich topic with directly relevant work. Here's a comprehensive answer:

Your design closely mirrors **duckflux**, a workflow DSL published in March 2026 that independently converged on nearly the same primitives you're describing — and it explicitly chose Google CEL for expressions for the same reasons you'd want.[^1_1]

## Prior Art \& Close Relatives

**duckflux** is the closest known match to your design. It uses YAML, JSON Schema for typed outputs, and Google CEL for all conditions and transition expressions. Its author explicitly documents why CEL was chosen over JS eval (security surface), custom mini-DSLs (implementation burden), and JSONPath/JMESPath (poor logic support). The key difference: duckflux treats workflows as sequential pipelines with loops/conditionals, not general CFGs with arbitrary block transitions.[^1_1]

**LangGraph** is another strong reference point — it models workflows as explicit state graphs where nodes are LLM calls or tools, and edges are conditional transitions. The state is typed, but conditions are Python functions, not a sandboxed expression language.[^1_2]

**Azure Prompt Flow** offers DAG-based node graphs with typed inputs/outputs, visual editing, and is designed specifically for LLM orchestration. It skips the CFG model (no cycles) and uses Python for conditions.[^1_3]

**PayPal's declarative LLM DSL** (arxiv:2512.19769) is a CFG-style pipeline DSL deployed in production at scale, reporting 60% reduction in dev time vs. imperative code. The paper is worth reading as it targets the same pattern: prompt + schema + routing logic.[^1_4]

## What Makes Your Design Distinctive

The specific combination you're targeting — **CFG (not just DAG) + structured output schema per block + CEL guard expressions** — hasn't been cleanly packaged as a standalone embeddable format. The CFG property (allowing cycles/backward edges) is what most tools deliberately exclude, since cycles in Airflow/Prefect/Argo are architecturally forbidden. duckflux adds cycles via `loop` constructs but doesn't expose raw CFG edge semantics.[^1_1]

## Grammar Sketch

Here's what an intuitive TOML-like or YAML DSL could look like for your model:

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

A few design notes on this grammar:

- **Guards are evaluated in order** — first truthy wins, so the fallback `"true"` belongs at the end, making the ordering semantics obvious.
- **`context`** is a mutable scratchpad (like duckflux's `execution.context`) for cross-block state like retry counts, distinct from `output` which is the structured LLM response for the current block.[^1_1]
- **Schemas are external JSON Schema files** or inline YAML objects — separating them keeps the prompt block readable.
- **Self-loops are first-class** (e.g., `billing_handler` → `billing_handler`) since you're modeling a CFG, not a DAG.
- CEL fits perfectly here: expressions like `output.resolved == true`, `context.attempts < 3`, or `"billing" in output.tags` are all valid CEL with no escape hatches.[^1_5][^1_1]


## Alternative Syntax: Rust-Friendly

If you're building this in Rust (likely given your background), a more compact syntax amenable to `nom` or `pest` parsing:

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

The `->` syntax makes transitions visually scannable and mirrors state machine conventions, which is ergonomic for the CFG mental model. CEL expressions follow `when`, and a bare `->` is the unconditional fallback.

## Crates to Know

For your Rust implementation:

- **`cel-interpreter`** or **`cel-rust`** — CEL evaluator crates for embedding Google CEL natively
- **`jsonschema`** — for validating LLM outputs against your per-block schemas
- **`serde_json` + `toml`/`serde_yaml`** — for deserializing the workflow spec
- **`pest`** or **`nom`** — if you want a custom grammar parser for the compact `->` syntax above rather than YAML/TOML
<span style="display:none">[^1_10][^1_11][^1_12][^1_13][^1_14][^1_15][^1_6][^1_7][^1_8][^1_9]</span>

<div align="center">⁂</div>

[^1_1]: https://ggondim.notion.site/duckflux-A-Declarative-Workflow-DSL-Born-from-the-Multi-Agent-Orchestration-Gap-31f9e3681d4880899242fb64ca78f882

[^1_2]: https://dev.to/rosidotidev/in-depth-comparison-workflow-control-with-langgraph-and-crewai-ae3

[^1_3]: https://learn.microsoft.com/en-us/azure/ai-foundry/concepts/prompt-flow

[^1_4]: https://arxiv.org/abs/2512.19769

[^1_5]: https://www.mintlify.com/agentgateway/agentgateway/concepts/cel-expressions

[^1_6]: https://docs.agentos.sh/features/workflow-dsl

[^1_7]: https://learn.microsoft.com/en-us/agent-framework/user-guide/workflows/declarative-workflows/expressions

[^1_8]: https://www.catalyzex.com/paper/a-declarative-language-for-building-and

[^1_9]: https://arxiv.org/html/2503.15520v1

[^1_10]: https://www.vitaliihonchar.com/insights/flow-run-system-design

[^1_11]: https://docs.agentgr.id/agents/agent-dsl-manager/

[^1_12]: https://www.mindstudio.ai/blog/structured-ai-coding-workflow-deterministic-agentic-nodes

[^1_13]: https://dev.to/ggondim/duckflux-a-declarative-workflow-dsl-born-from-the-multi-agent-orchestration-gap-4n28

[^1_14]: https://learn.microsoft.com/en-us/azure/logic-apps/workflow-definition-language-schema

[^1_15]: https://www.sciencedirect.com/science/article/abs/pii/S0926580525002845


---

# I actually think YAML is more readable/understood by the users of my tool

Good call — YAML is universally understood and has great Rust support via `serde_yaml`. Here's the same grammar rewritten idiomatically in YAML:

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


## Key Design Decisions

**Inline vs. external schemas** — Inlining the JSON Schema directly as YAML (as above) keeps the workflow self-contained and readable. But for large, reused schemas you can also support a `$ref` string:

```yaml
schema: "./schemas/resolution.json"
```

Your deserializer can detect `string` vs. `object` for the `schema` field and load accordingly.

**Fallback transition** — A transition entry with no `when` key is the unconditional fallback. Evaluated last-wins-by-position is predictable and matches how people read YAML lists top-to-bottom. This is cleaner than a magic `when: "true"` CEL literal.

**Template variables** — Three scopes worth distinguishing explicitly in your docs:


| Variable prefix | Meaning |
| :-- | :-- |
| `{{input.*}}` | The workflow's initial input payload |
| `{{output.*}}` | The structured JSON output of the *current* block |
| `{{context.*}}` | Mutable cross-block scratchpad (retry counts, accumulated history, etc.) |

**Terminal detection** — A block with no `transitions` key (or an empty list) is implicitly terminal. No special `type: terminal` annotation needed; the absence is self-documenting.

## Rust Deserialization Sketch

With `serde` this maps very cleanly:

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

The `guard: Option<String>` maps directly to your CEL evaluation logic — `None` short-circuits to `true`, and you compile the `Some(expr)` strings into CEL programs at workflow load time rather than at each step, so you catch syntax errors early.

---

# This is very elegant.

Would it be possible to model Control TaskFlow Graph (CTFG) and/or Dataflow Graphs in a similar way to describe parallel agent workflows?

Great question — both are very modelable in YAML with modest extensions to what you already have.

## Control Taskflow Graphs (CTFGs)

A CTFG adds **fork/join** semantics — a block can spawn multiple parallel branches, and execution resumes when some condition over those branches is met (all complete, any complete, N-of-M, etc.). This maps naturally as a special block type:

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
    # no transitions — result is collected by the join

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

Notice `{{blocks.<name>.output.*}}` — a new scope for accessing named upstream block outputs, which is essential in parallel graphs where `{{output.*}}` is ambiguous.

The fork block itself carries no prompt or schema — it's a pure **control node**. The `join` strategy gives you the CTFG's synchronization semantics cleanly:


| Strategy | Meaning |
| :-- | :-- |
| `all` | Wait for every branch to complete |
| `any` | Resume as soon as the first branch completes (cancel others) |
| `n_of_m` | Resume when `n` of the `m` branches complete |

## Dataflow Graphs

A dataflow model flips the perspective — instead of edges being control transitions, **edges are data dependencies**. A block becomes ready to execute as soon as all its declared inputs are available. This is closer to how Dask, TensorFlow graphs, or Make work:

```yaml
workflow:
  mode: dataflow          # signals the executor to use dependency scheduling
  entry: [extract_facts, extract_entities]   # multiple roots possible

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
    depends_on: [extract_facts, extract_entities]   # fires when both are ready
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
    depends_on: [extract_facts]    # only needs facts, runs independently of entities
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

Here, `depends_on` replaces `transitions` entirely. Your executor topologically sorts the graph at load time, detects independent subgraphs, and schedules them concurrently with no extra author effort.

## Unified Model

The elegant realization is that all three models share the same block primitive — they only differ in how edges are expressed:


| Model | Edge mechanism | Cycles | Parallelism |
| :-- | :-- | :-- | :-- |
| **CFG** | `transitions` + CEL guards | ✅ | ❌ |
| **CTFG** | `fork`/`join` control nodes | ✅ | ✅ explicit |
| **Dataflow** | `depends_on` + `mode: dataflow` | ❌ | ✅ implicit |

You could support all three in the same runtime — a CFG block that `fork`s into parallel branches and later a `dataflow` subgraph is a legitimate mixed-mode workflow. The executor just needs to handle three node kinds: **prompt nodes** (have a `prompt` + `schema`), **fork nodes** (pure control, no LLM call), and **join nodes** (implicit — materialized at the `goto` target of a fork's join). The `depends_on` dataflow model is then syntactic sugar that gets compiled into fork/join pairs at load time.

---

# In the dataflow example, I notice that you provided the ‘input’ blocks as entry points. Is’t that counter intuitive for a dataflow graph? Wouldn’t you normally specify the ‘output’ and basically pull out the result?

You're absolutely right, and this is a sharp observation. The two evaluation strategies are well-defined in dataflow theory:[^4_1]

- **Data-driven (eager/push):** nodes fire as soon as inputs are available — you specify *sources* and data propagates forward. This is what my example used.
- **Demand-driven (lazy/pull):** activation propagates *backward* from a requested output — you specify the *sink* you want and the graph resolves dependencies on demand.

For an LLM workflow harness, **pull semantics are genuinely more intuitive** from the author's perspective. You declare what you want, not what to start from.

## Pull-Oriented Dataflow YAML

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

The key change: `workflow.output` names the sink, and `entry` disappears entirely. Your executor resolves the graph by walking `depends_on` edges backward from that sink, building an execution plan, then scheduling nodes eagerly in topological order once the plan is known. The author never thinks about roots — they think about what they want.

## Why This Is Better

The pull model also gives you two practical benefits for free:

**1. Dead node elimination.** Any block not reachable by walking backward from `output` is simply never executed — just like a compiler dropping unreachable code. This is useful during development when you're iterating on a workflow and leave old blocks around.

**2. Multiple outputs are natural.** You can extend `output` to a list if you want to materialise several results from one workflow run:

```yaml
workflow:
  mode: dataflow
  output: [final_report, score_facts]   # both are sinks; shared deps run once
```

The shared `extract_facts` node runs exactly once and fans out to both sinks — no duplication, no coordination code needed.

## Execution Model Summary

```
Author declares:   workflow.output → "final_report"
Executor does:
  1. Walk depends_on backward → build DAG
  2. Topo-sort → find execution levels
  3. Execute level-by-level, parallelising within each level
  4. Return the output of the declared sink(s)
```

This is the same model Make, Dask, and Bazel use — which is exactly why it feels intuitive. The user thinks in terms of *artifacts they want*, not *processes they must start*.
<span style="display:none">[^4_10][^4_11][^4_12][^4_13][^4_14][^4_15][^4_2][^4_3][^4_4][^4_5][^4_6][^4_7][^4_8][^4_9]</span>

<div align="center">⁂</div>

[^4_1]: https://www.sciencedirect.com/topics/computer-science/data-flow-graph

[^4_2]: https://www.visual-paradigm.com/guide/data-flow-diagram/what-is-data-flow-diagram/

[^4_3]: https://en.wikipedia.org/wiki/Dataflow_programming

[^4_4]: https://redplanetlabs.com/docs/~/tutorial4.html

[^4_5]: https://devopedia.org/dataflow-programming

[^4_6]: https://www.stce.rwth-aachen.de/files/elearning/DataFlowReversal_I.pdf

[^4_7]: https://arxiv.org/pdf/2303.09552.pdf

[^4_8]: https://rcgoncalves.pt/files/publications/Goncalves-2014b.pdf

[^4_9]: https://www.visual-paradigm.com/features/data-flow-diagram-tool/

[^4_10]: https://staff.cs.upt.ro/~petrum/uploads/Main/Mihancea-SYNASC08.pdf

[^4_11]: https://www.epatters.org/assets/papers/2017-dataflow-graphs-ibmjrd.pdf

[^4_12]: https://www.stce.rwth-aachen.de/files/elearning/DataFlowReversal_II.pdf

[^4_13]: https://stackoverflow.com/questions/41533195/visual-paradigm-data-flow-diagrams

[^4_14]: https://www.cs.utexas.edu/ftp/predator/15SoSymExt.pdf

[^4_15]: https://docs.bytewax.io/stable/guide/concepts/dataflow-programming.html

