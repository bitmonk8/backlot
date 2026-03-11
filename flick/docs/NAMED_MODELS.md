# Flick Named Models Spec

Status: **proposal** — for implementation in Flick.

## Problem

Flick's `Config` requires every call to fully specify the model's provider, model ID, max_tokens, and temperature inline:

```yaml
model:
  provider: anthropic
  name: claude-sonnet-4-6
  max_tokens: 8192
  temperature: 0.0

provider:
  anthropic:
    api: messages
    credential: my-key
```

This is fine for Flick's CLI (one config file per invocation), but creates problems for library consumers like Reel and Epic that make many calls with different models in a single session:

1. **Repetition**: Epic's `config_gen.rs` rebuilds the full JSON config for every agent call. Provider block, credential, temperature — all identical every time. Only model name and max_tokens vary per tier.

2. **Scattered tier definitions**: Epic defines `default_max_tokens(Model)` and `resolve_model_name(Model, &ModelConfig)` in its own code because Flick has no place to put "Sonnet means this model ID with these settings." The tier abstraction leaks into every consumer.

3. **Missing indirection layer**: Flick has `provider` (named, reusable) but `model` is inline and anonymous. The config has a `provider` map but no `model` map. This asymmetry means consumers cannot name and reuse model configurations.

## Proposed Change

Add a `models` map to Flick's config, parallel to the existing `provider` map. Each entry is a named model configuration. The top-level `model` field becomes either an inline definition (backward-compatible) or a string reference to a named model.

### Config Format

```yaml
# Named model definitions (new)
models:
  fast:
    provider: anthropic
    name: claude-haiku-4-5-20251001
    max_tokens: 8192
    temperature: 0.0
  balanced:
    provider: anthropic
    name: claude-sonnet-4-6
    max_tokens: 8192
    temperature: 0.0
  strong:
    provider: anthropic
    name: claude-opus-4-6
    max_tokens: 16384
    temperature: 0.0

# Active model selection — reference by name
model: balanced

# Provider definitions (unchanged)
provider:
  anthropic:
    api: messages
    credential: anthropic
```

### Backward Compatibility

The `model` field accepts two forms:

1. **String** (new) — references a key in the `models` map:
   ```yaml
   model: balanced
   ```

2. **Object** (existing) — inline model definition, works exactly as today:
   ```yaml
   model:
     provider: anthropic
     name: claude-sonnet-4-6
   ```

When `model` is a string, `models` must contain a matching key. When `model` is an object, `models` is not required (the inline definition is used directly).

### Struct Changes

```rust
// New: named model map
#[derive(Debug, Deserialize)]
pub struct Config {
    // Deserialized as either String or ModelConfig via custom deserializer
    model: ModelRef,

    #[serde(default)]
    models: HashMap<String, ModelConfig>,

    // ... existing fields unchanged ...
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    output_schema: Option<OutputSchema>,
    #[serde(default)]
    provider: HashMap<String, ProviderConfig>,
    #[serde(default)]
    tools: Vec<ToolConfig>,
    #[serde(default)]
    pricing: Option<PricingConfig>,
}

/// Either an inline model definition or a reference to a named model.
#[derive(Debug)]
enum ModelRef {
    Inline(ModelConfig),
    Named(String),
}
```

### Resolution

`Config::model()` resolves the active model:
- If `ModelRef::Inline(config)` → return `&config`
- If `ModelRef::Named(name)` → look up in `self.models`, error if missing

```rust
impl Config {
    pub fn model(&self) -> Result<&ModelConfig, ConfigError> {
        match &self.model {
            ModelRef::Inline(config) => Ok(config),
            ModelRef::Named(name) => self.models.get(name).ok_or_else(|| {
                ConfigError::UnknownModel(name.clone())
            }),
        }
    }
}
```

**Breaking change**: `model()` currently returns `&ModelConfig` infallibly (it's `const`). With named models, resolution can fail. The return type changes to `Result<&ModelConfig, ConfigError>`.

Alternative: resolve during `validate()` and store the resolved reference, keeping `model()` infallible. This is cleaner but requires interior mutability or a resolved-config wrapper type. Recommend resolving in `validate()` and caching a reference or index.

### CLI Override

The existing `--model` flag overrides the model ID within the resolved `ModelConfig`. It does not select a different named model. A new `--use-model <name>` flag could select a named model, but this is optional for v1.

### Validation

During `validate()`:
- If `model` is `Named(name)`, verify `name` exists in `models`
- Each entry in `models` validates the same way `ModelConfig` does today (non-empty name, valid temperature, etc.)
- Each entry's `provider` field must reference a key in the `provider` map
- `models` entries that are not referenced by `model` are still validated (they may be used by library consumers selecting models dynamically)

### Library API: Selecting a Named Model

For library consumers (Reel, Epic), the key capability is switching models without rebuilding the config:

```rust
impl Config {
    /// Switch the active model to a named model from the `models` map.
    /// Re-validates; reverts on failure.
    pub fn select_model(&mut self, name: &str) -> Result<(), ConfigError> {
        // ...
    }
}
```

This is the primary motivator. Epic builds one `Config` at startup with all three tiers defined in `models`. Each agent call just calls `config.select_model("fast")` or `config.select_model("balanced")` instead of rebuilding the entire config from JSON.

## Impact on Consumers

### Reel

Reel's `AgentConfig` holds a single `flick::Config` (built once). `AgentRequest` specifies a model name (string). Reel calls `config.select_model(&request.model)` before each agent call. No JSON rebuilding.

```rust
// Reel internals (sketch)
pub struct AgentConfig {
    flick_config: flick::Config,  // built once with models + providers
    // ...
}

pub struct AgentRequest {
    pub model: String,  // references a key in flick_config.models
    // ...
}

impl Agent {
    pub async fn run<T>(&self, request: AgentRequest) -> Result<RunResult<T>> {
        let mut config = self.config.flick_config.clone();  // or select_model on a &mut
        config.select_model(&request.model)?;
        // ...
    }
}
```

### Epic

Epic's `config_gen.rs` simplifies dramatically. Instead of 8 config builder functions that each rebuild JSON, epic builds one `flick::Config` at startup and passes model names per-call:

```rust
// Before (epic today): ~80 lines of JSON building per config
let json = json!({
    "model": { "provider": "anthropic", "name": model_name, "max_tokens": 8192, "temperature": 0.0 },
    "provider": { "anthropic": { "api": "messages", "credential": cred } },
    "tools": [...],
    "output_schema": { "schema": ... }
});
let config = flick::Config::from_str(&json_str, Json)?;

// After: build once, select per-call
let mut config = base_config.clone();
config.select_model("balanced")?;
// set tools, output_schema, system_prompt per-call
```

### Flick CLI

No change to existing configs. Inline `model` still works. Users who want named models can define a `models` map. The CLI `--model` flag overrides the model ID within the selected model, as today.

## Per-Call Overrides

With named models handling the model/provider/max_tokens bundle, the remaining per-call fields are:

| Field | Varies per call? | Mechanism |
|---|---|---|
| model selection | Yes | `select_model("name")` |
| system_prompt | Yes | Needs override method |
| output_schema | Yes | Needs override method |
| tools | Yes | Needs override method |
| pricing | No | Set once |

Flick needs override methods for system_prompt, output_schema, and tools — or alternatively, a builder pattern that layers per-call fields onto a base config. This is secondary to the named models change but worth considering together.

Flick already has `override_model_name()` and `override_reasoning()`. Adding `override_system_prompt()`, `override_tools()`, and `override_output_schema()` follows the same pattern.

## Summary

| Change | Scope |
|---|---|
| Add `models: HashMap<String, ModelConfig>` to `Config` | Flick config.rs |
| Make `model` field accept string or object | Flick config.rs (custom deser) |
| Add `Config::select_model(&mut self, name: &str)` | Flick config.rs |
| Add override methods for system_prompt, tools, output_schema | Flick config.rs |
| Add `ConfigError::UnknownModel(String)` variant | Flick error.rs |
| Validation of models map entries | Flick config.rs |
| Update CLI `--model` semantics (optional) | Flick CLI |
| Update existing tests, add named model tests | Flick config.rs tests |
