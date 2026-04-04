# Flick Named Models Spec

## Five Types, Five Concerns

| Type | Responsibility | Storage |
|---|---|---|
| `ProviderRegistry` | Map of name → `ProviderInfo` | `~/.flick/providers` |
| `ProviderInfo` | API type, base URL, encrypted credential | Entry in ProviderRegistry |
| `ModelRegistry` | Map of name → `ModelInfo` | `~/.flick/models` |
| `ModelInfo` | Provider ref, model ID, max_tokens, pricing | Entry in ModelRegistry |
| `RequestConfig` | Model ref, system_prompt, tools, output_schema, temperature, reasoning | Per-invocation YAML/JSON file |

### Resolution Chain

```
RequestConfig.model ("balanced")
    → ModelRegistry["balanced"] → ModelInfo { provider: "anthropic", name: "claude-sonnet-4-6", ... }
        → ProviderRegistry["anthropic"] → ProviderInfo { api: messages, base_url: "https://api.anthropic.com", ... }
```

Each layer references the next by string key. Resolution happens once at client construction time.

### ProviderRegistry

Stored at `~/.flick/providers`.

```toml
[anthropic]
api = "messages"
base_url = "https://api.anthropic.com"
key = "enc3:..."   # encrypted API key

[openrouter]
api = "chat_completions"
base_url = "https://openrouter.ai"
key = "enc3:..."
compat.explicit_tool_choice_auto = true
```

```rust
pub struct ProviderRegistry {
    providers: HashMap<String, ProviderInfo>,
}

pub struct ProviderInfo {
    pub api: ApiKind,
    pub base_url: String,
    pub key: String,               // encrypted on disk, decrypted in memory
    pub compat: Option<CompatFlags>,
}
```

Compatibility flags (`compat`) are properties of the provider endpoint, not the request.

### ModelRegistry

Stored at `~/.flick/models`.

```toml
[fast]
provider = "anthropic"
name = "claude-haiku-4-5-20251001"
max_tokens = 8192
input_per_million = 0.80
output_per_million = 4.00

[balanced]
provider = "anthropic"
name = "claude-sonnet-4-6"
max_tokens = 8192
input_per_million = 3.00
output_per_million = 15.00

[strong]
provider = "anthropic"
name = "claude-opus-4-6"
max_tokens = 16384
input_per_million = 15.00
output_per_million = 75.00
```

```rust
pub struct ModelRegistry {
    models: HashMap<String, ModelInfo>,
}

pub struct ModelInfo {
    pub provider: String,          // key into ProviderRegistry
    pub name: String,              // model ID as known by the provider API
    pub max_tokens: Option<u32>,
    pub input_per_million: Option<f64>,
    pub output_per_million: Option<f64>,
}
```

`provider` must reference a key in the ProviderRegistry. `name` is the actual model identifier sent to the API (e.g. `claude-sonnet-4-6`, `gpt-4o`). Pricing is optional. The registry is purely user-defined — no builtin/hardcoded models.

### RequestConfig

Per-invocation YAML/JSON file deserializes into this type.

```yaml
model: balanced
system_prompt: "You are a code assistant."
temperature: 0.0
reasoning:
  level: medium
output_schema:
  schema:
    type: object
    properties:
      answer:
        type: string
tools:
  - name: read_file
    description: "Read a file's contents"
    parameters:
      type: object
      properties:
        path:
          type: string
      required: [path]
```

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestConfig {
    model: String,                          // key into ModelRegistry
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    reasoning: Option<ReasoningConfig>,
    #[serde(default)]
    output_schema: Option<OutputSchema>,
    #[serde(default)]
    tools: Vec<ToolConfig>,
}
```

No `provider` block. No `pricing`. No model ID. Just a model name (resolved through registries) and per-request parameters.

`temperature` and `reasoning` live here because the same model may be called with different settings depending on the task.

### FlickClient Construction

The client resolves the full chain at construction time:

```rust
impl FlickClient {
    pub fn new(
        request: RequestConfig,
        models: &ModelRegistry,
        providers: &ProviderRegistry,
    ) -> Result<FlickClient, FlickError> {
        // 1. Resolve model name → ModelInfo
        // 2. Resolve ModelInfo.provider → ProviderInfo
        // 3. Build HTTP provider from ProviderInfo
        // 4. Validate the full resolved config
        // ...
    }
}
```

Resolution errors (unknown model name, unknown provider reference) fail at construction, not at call time.

### CLI Flow

```
1. Load ~/.flick/providers     → ProviderRegistry
2. Load ~/.flick/models        → ModelRegistry
3. Parse request YAML/JSON     → RequestConfig
4. FlickClient::new(request, &models, &providers)
5. client.run(query, &mut context)
```

### CLI Commands

`flick provider add <name>` — interactive, prompts for API key, API type, base URL. Writes to `~/.flick/providers`. Replaces current `flick setup`.

`flick provider list` — lists providers (reads `~/.flick/providers`).

`flick model add <name>` — interactive, prompts for provider, model ID, max_tokens, pricing. Writes to `~/.flick/models`.

`flick model list` — lists entries in `~/.flick/models`.

`flick model remove <name>` — removes an entry from `~/.flick/models`.

`flick init` — interactive, generates a RequestConfig YAML file. Prompts for model name (key from ModelRegistry), system prompt. If ModelRegistry is empty, directs user to `flick model add` first.

`flick run` — no CLI override flags (`--model`, `--temperature`, `--reasoning` removed). The RequestConfig file is the sole source of request parameters.

### Library Usage

```rust
use flick::{RequestConfig, ConfigFormat, ModelRegistry, ProviderRegistry, FlickClient, Context};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load registries (once at startup)
    let providers = ProviderRegistry::load_default()?;
    let models = ModelRegistry::load_default()?;

    // Parse request config
    let yaml = std::fs::read_to_string("request.yaml")?;
    let request = RequestConfig::from_str(&yaml, ConfigFormat::Yaml)?;

    // Build client (resolves model → provider chain)
    let client = FlickClient::new(request, &models, &providers)?;

    let mut ctx = Context::default();
    let result = client.run("What is Rust?", &mut ctx).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
```

For library consumers switching models across calls:

```rust
// Epic/Reel: build one set of registries, vary RequestConfig per call
let providers = ProviderRegistry::load_default()?;
let models = ModelRegistry::load_default()?;

// Fast model call
let request = RequestConfig::builder()
    .model("fast")
    .system_prompt("Triage this issue.")
    .build()?;
let client = FlickClient::new(request, &models, &providers)?;

// Strong model call
let request = RequestConfig::builder()
    .model("strong")
    .system_prompt("Write a detailed implementation plan.")
    .tools(planning_tools)
    .build()?;
let client = FlickClient::new(request, &models, &providers)?;
```

### Validation

**ProviderRegistry** (validated on load):
- Each entry has a non-empty `api` and `base_url`
- Key decryption is deferred until provider is actually used

**ModelRegistry** (validated on load):
- Each entry has a non-empty `name`
- `max_tokens` if present must be > 0
- Pricing values if present must be non-negative and finite
**Cross-registry validation** (`validate_registries(&ModelRegistry, &ProviderRegistry)`):
- Called once after both registries are loaded, before any FlickClient construction
- Every `ModelInfo.provider` must reference an existing key in the ProviderRegistry
- May grow additional checks over time
- FlickClient construction panics if provider is not found (assumes validation already ran)

**RequestConfig** (validated at FlickClient construction):
- `model` references a key in ModelRegistry
- `temperature` is non-negative and finite, within API-specific ceiling
- `reasoning` + `output_schema` mutual exclusion (Messages API)
- `budget_tokens` < `max_tokens` constraint (Anthropic with reasoning)
- Tool names non-empty and unique
- Tool descriptions non-empty
- Tool parameters are JSON objects if present

## Design Rationale

- `model` field in RequestConfig is always a string key into ModelRegistry. No inline model definitions.
- TOML for both registries.
- No builtin models. ModelRegistry is purely user-defined.
- No CLI override flags. The RequestConfig file is the sole source of request parameters.
- Builder pattern for programmatic RequestConfig construction.
- `validate_registries()` checks cross-registry reference integrity after both registries are loaded. FlickClient construction assumes validation already ran.
- `flick init` generates RequestConfig only. Directs user to `flick model add` / `flick provider add` if registries are empty.
