# Flick Integration

## Status

**Implemented.** Flick integrated as a library dependency (crate).

## Role

Flick serves as the **agent runtime** — a Rust library that Epic calls directly for each agent interaction. Epic is the **orchestrator** — it decides what work to do, which model to use, which tools to grant, and what system prompt to send. Flick executes individual agent sessions via its `FlickClient` API; Epic manages the recursive task tree.

## Integration Mode: Library

Epic depends on Flick as a git crate dependency (`flick = { git = "..." }`). Agent calls use `FlickClient::run()` and `FlickClient::resume()` directly — no process spawning, no YAML config files on disk, no tool result files.

### API Flow

```
Epic
 |
 |-- Build flick::Config from JSON in-memory
 |-- resolve_provider(&config).await
 |-- FlickClient::new(config, provider)
 |-- client.run(query, &mut context).await
 |
 |   (if ToolCallsPending)
 |-- Execute tools locally
 |-- Build Vec<ContentBlock::ToolResult>
 |-- client.resume(&mut context, tool_results).await
 |   (repeat until Complete)
 |
 |-- Extract text from FlickResult.content
```

### Key Types

| Epic usage | Flick type |
|---|---|
| Config construction | `flick::Config` (parsed from JSON via `Config::from_str`) |
| Client | `flick::FlickClient` |
| Conversation state | `flick::Context` |
| Response | `flick::FlickResult` |
| Content blocks | `flick::ContentBlock` (Text, Thinking, ToolUse, ToolResult) |
| Result status | `flick::result::ResultStatus` (Complete, ToolCallsPending, Error) |
| Provider resolution | `flick::resolve_provider()` |
| Errors | `flick::FlickError` |

### Timeout Handling

`tokio::time::timeout` wraps `client.run()` and `client.resume()`. On timeout, the HTTP request inside Flick is dropped (reqwest future cancelled).

### Credential Management

Flick's `CredentialStore` (default `~/.flick/`) resolves API keys. The credential name is passed via Epic's `--credential` CLI option (default: `anthropic`).

## Repository

- [github.com/bitmonk8/flick](https://github.com/bitmonk8/flick)

## History

### Previous: Subprocess invocation

Before library migration, Epic invoked Flick as an external executable — writing YAML config files to disk, spawning `flick run --config ... --query ...`, parsing JSON from stdout, and writing tool result files for `--resume` calls. The library migration eliminated all file I/O and process management overhead.

### Previous: ZeroClaw

ZeroClaw (library dependency via forked submodule) was the original agent runtime, replaced by Flick due to provenance concerns, heavy dependency tree (~771 packages), and intermittent compiler ICE.
