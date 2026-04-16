// mech CLI: thin binary over the mech library.
//
// Subcommands:
//   mech validate <workflow>  - load and validate a workflow file
//   mech run <workflow> [--function <name>] --input <json>  - run a workflow

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use mech::{
    AgentExecutor, AgentRequest, AgentResponse, BoxFuture, MechError, WorkflowLoader,
    WorkflowRuntime,
};
use serde_json::Value as JsonValue;

// ---------------------------------------------------------------------------
// CLI argument types
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "mech", about = "Mech workflow engine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate a workflow file without executing it.
    Validate {
        /// Path to the workflow YAML file.
        workflow: PathBuf,
    },
    /// Run a workflow.
    Run {
        /// Path to the workflow YAML file.
        workflow: PathBuf,
        /// Entry function name (default: first function).
        #[arg(long)]
        function: Option<String>,
        /// Input JSON string.
        #[arg(long)]
        input: String,
    },
}

// ---------------------------------------------------------------------------
// CLI-local error type
// ---------------------------------------------------------------------------

/// CLI-local error enum — wraps library errors and adds CLI-specific variants
/// for JSON input parsing and output serialization.
#[derive(Debug)]
enum CliError {
    /// A mech library error.
    Mech(MechError),
    /// Bad JSON input from the user (e.g., `--input` flag).
    InputParse { message: String },
    /// Failed to serialize output JSON.
    OutputSerialize { message: String },
}

impl From<MechError> for CliError {
    fn from(err: MechError) -> Self {
        CliError::Mech(err)
    }
}

// ---------------------------------------------------------------------------
// Stub agent executor
// ---------------------------------------------------------------------------

struct StubAgent;

impl AgentExecutor for StubAgent {
    fn run<'a>(
        &'a self,
        _request: AgentRequest,
    ) -> BoxFuture<'a, Result<AgentResponse, MechError>> {
        Box::pin(async {
            Err(MechError::LlmCallFailure {
                block: String::new(),
                message: "standalone CLI does not have an agent executor configured".into(),
            })
        })
    }
}

// ---------------------------------------------------------------------------
// Error formatting
// ---------------------------------------------------------------------------

fn print_error(err: &CliError) {
    match err {
        CliError::Mech(MechError::WorkflowValidation { errors }) => {
            for e in errors {
                eprintln!("error: {e}");
            }
        }
        CliError::Mech(other) => {
            eprintln!("error: {other}");
        }
        CliError::InputParse { message } => {
            eprintln!("error: bad input JSON: {message}");
        }
        CliError::OutputSerialize { message } => {
            eprintln!("error: output serialization failed: {message}");
        }
    }
}

// ---------------------------------------------------------------------------
// Command implementations
// ---------------------------------------------------------------------------

async fn run_validate(workflow_path: &Path) -> Result<(), CliError> {
    WorkflowLoader::new().load(workflow_path)?;
    Ok(())
}

async fn run_execute(
    workflow_path: &Path,
    function: Option<String>,
    input_json: &str,
) -> Result<(), CliError> {
    let input: JsonValue = serde_json::from_str(input_json).map_err(|e| CliError::InputParse {
        message: e.to_string(),
    })?;

    let workflow = WorkflowLoader::new().load(workflow_path)?;
    let agent = StubAgent;
    let runtime = WorkflowRuntime::new(&workflow, &agent);

    let entry_fn = match &function {
        Some(name) => name.clone(),
        None => runtime.default_entry_function()?.to_owned(),
    };

    let output = runtime.run(&entry_fn, input).await?;

    let pretty = serde_json::to_string_pretty(&output).map_err(|e| CliError::OutputSerialize {
        message: e.to_string(),
    })?;
    println!("{pretty}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    let result = match &cli.command {
        Command::Validate { workflow } => run_validate(workflow).await,
        Command::Run {
            workflow,
            function,
            input,
        } => run_execute(workflow, function.clone(), input).await,
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            print_error(&err);
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // ---- T1: validate good workflow succeeds --------------------------------

    #[test]
    fn validate_good_workflow_succeeds() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let example_path =
            std::path::Path::new(manifest_dir).join("../mech/src/schema/full_example.yaml");
        let result = WorkflowLoader::new().load(&example_path);
        assert!(result.is_ok(), "expected Ok but got: {:?}", result.err());
    }

    // ---- T2: validate broken workflow fails ---------------------------------

    #[test]
    fn validate_broken_workflow_fails() {
        use std::io::Write as _;
        use tempfile::NamedTempFile;

        let broken_yaml = r#"
functions:
  my_func:
    input: { type: object }
    blocks:
      step1:
        prompt: "hello"
        schema:
          type: object
          required: [k]
          properties:
            k: { type: string }
        transitions:
          - goto: nonexistent_block
"#;
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "{broken_yaml}").unwrap();
        let result = WorkflowLoader::new().load(tmp.path());
        assert!(result.is_err(), "expected Err but got Ok");
        match result.unwrap_err() {
            MechError::WorkflowValidation { errors } => {
                assert!(!errors.is_empty(), "expected at least one validation error");
            }
            other => panic!("expected WorkflowValidation error but got: {other:?}"),
        }
    }

    // ---- T3: run trivial workflow with stub agent errors --------------------

    #[tokio::test]
    async fn run_trivial_workflow_with_stub_agent_errors() {
        use std::io::Write as _;
        use tempfile::NamedTempFile;

        let yaml = r#"
functions:
  greet:
    input: { type: object }
    blocks:
      say_hello:
        prompt: "Say hello."
        schema:
          type: object
          required: [message]
          properties:
            message: { type: string }
"#;
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "{yaml}").unwrap();

        let result = run_execute(tmp.path(), None, "{}").await;
        assert!(result.is_err(), "expected Err from stub agent");
        match result.unwrap_err() {
            CliError::Mech(MechError::LlmCallFailure { message, .. }) => {
                assert!(
                    message.contains("standalone CLI"),
                    "expected 'standalone CLI' in message, got: {message}"
                );
            }
            other => panic!("expected LlmCallFailure but got: {other:?}"),
        }
    }

    // ---- T4: missing workflow file produces Io error -----------------------

    #[test]
    fn cli_missing_workflow_file() {
        let result = WorkflowLoader::new().load("nonexistent_workflow_12345.yaml");
        assert!(result.is_err());
        match result.unwrap_err() {
            MechError::Io { .. } => {}
            other => panic!("expected MechError::Io but got: {other:?}"),
        }
    }

    // ---- T5: bad input JSON returns error ----------------------------------

    #[tokio::test]
    async fn cli_bad_input_json() {
        use std::io::Write as _;
        use tempfile::NamedTempFile;

        let yaml = r#"
functions:
  greet:
    input: { type: object }
    blocks:
      step:
        prompt: "Hi."
        schema:
          type: object
          required: [k]
          properties:
            k: { type: string }
"#;
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "{yaml}").unwrap();

        let result = run_execute(tmp.path(), None, "not-json").await;
        assert!(result.is_err(), "expected error for bad JSON input");
        match result.unwrap_err() {
            CliError::InputParse { .. } => {}
            other => panic!("expected CliError::InputParse for bad JSON but got: {other:?}"),
        }
    }

    // ---- T6: default entry function returns first alphabetically ------------

    #[test]
    fn cli_default_entry_function() {
        use std::io::Write as _;
        use tempfile::NamedTempFile;

        let yaml = r#"
functions:
  zebra:
    input: { type: object }
    blocks:
      z:
        prompt: "Z."
        schema:
          type: object
          required: [v]
          properties:
            v: { type: string }
  alpha:
    input: { type: object }
    blocks:
      a:
        prompt: "A."
        schema:
          type: object
          required: [v]
          properties:
            v: { type: string }
  middle:
    input: { type: object }
    blocks:
      m:
        prompt: "M."
        schema:
          type: object
          required: [v]
          properties:
            v: { type: string }
"#;
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "{yaml}").unwrap();

        let workflow = WorkflowLoader::new().load(tmp.path()).unwrap();
        let agent = StubAgent;
        let runtime = WorkflowRuntime::new(&workflow, &agent);
        let entry = runtime.default_entry_function().unwrap();
        // BTreeMap iterates alphabetically, so "alpha" comes first.
        assert_eq!(entry, "alpha");
    }

    // ---- T7: validate surfaces all errors -----------------------------------

    #[test]
    fn validate_prints_all_errors() {
        use std::io::Write as _;
        use tempfile::NamedTempFile;

        // Two functions each with a transition to a nonexistent block.
        let yaml = r#"
functions:
  func_a:
    input: { type: object }
    blocks:
      step1:
        prompt: "A."
        schema:
          type: object
          required: [k]
          properties:
            k: { type: string }
        transitions:
          - goto: missing_block_x
  func_b:
    input: { type: object }
    blocks:
      step2:
        prompt: "B."
        schema:
          type: object
          required: [k]
          properties:
            k: { type: string }
        transitions:
          - goto: missing_block_y
"#;
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "{yaml}").unwrap();

        let result = WorkflowLoader::new().load(tmp.path());
        assert!(result.is_err());
        match result.unwrap_err() {
            MechError::WorkflowValidation { errors } => {
                assert!(
                    errors.len() >= 2,
                    "expected at least 2 errors, got: {errors:?}"
                );
            }
            other => panic!("expected WorkflowValidation error but got: {other:?}"),
        }
    }
}
