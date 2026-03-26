# Vault

Persistent, file-based knowledge store for agent systems. Accumulates project knowledge (research, discoveries, design decisions, findings) and exposes it through structured operations. Designed as a standalone library consumed by orchestrators such as epic.

Access is serialized by the orchestrator; vault does not handle concurrent access internally.

## Sibling Projects

| Project | Role | Repository |
|---|---|---|
| **epic** | Orchestrator that consumes vault as its knowledge store | [github.com/bitmonk8/epic](https://github.com/bitmonk8/epic) |
| **reel** | Agent session layer; the librarian is implemented as a reel agent | [github.com/bitmonk8/reel](https://github.com/bitmonk8/reel) |
| **lot** | OS-level sandbox; enforces file access grants at the process boundary | [github.com/bitmonk8/lot](https://github.com/bitmonk8/lot) |
| **vault** | This project | [github.com/bitmonk8/vault](https://github.com/bitmonk8/vault) |

## Configuration

The CLI reads a YAML config file via `--config <path>`:

```yaml
storage_root: ".epic/docs/"
models:
  bootstrap: "sonnet"
  query: "haiku"
  record: "haiku"
  reorganize: "sonnet"
```

`models` allows the orchestrator to choose which model handles each operation.

## CLI Usage

### `vault bootstrap`

```
vault bootstrap --config <path>
```

Reads requirements from stdin. Creates the initial vault structure.

### `vault query`

```
vault query --config <path> --query <text>
vault query --config <path> < question.txt
```

Query text via `--query` flag or stdin. Outputs `QueryResult` as JSON to stdout.

### `vault record`

```
vault record --config <path> --name <NAME> --mode new|append
vault record --config <path> --name <NAME> --mode new|append --content <text>
```

Content via `--content` flag or stdin. `--name` is the document base name (e.g., `FINDINGS`). `--mode` is required. Outputs modified documents as JSON.

### `vault reorganize`

```
vault reorganize --config <path>
```

Triggers a full restructuring pass. Outputs `ReorganizeReport` as JSON.

## Output

All subcommands emit JSON to stdout on success. Errors are emitted as JSON to stderr with a non-zero exit code.
