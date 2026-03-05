// `epic init` — agent-driven interactive configuration scaffolding.

use crate::agent::config_gen::{DetectedStepWire, InitFindingsWire};
use crate::agent::flick::FlickAgent;
use crate::config::project::{EpicConfig, LimitsConfig, ModelConfig, VerificationStep};
use anyhow::bail;
use std::fmt::Write as FmtWrite;
use std::io::{self, BufRead, Write};
use std::path::Path;

/// Run the full init flow: agent exploration + interactive confirmation + write epic.toml.
pub async fn run_init(agent: &FlickAgent, project_root: &Path) -> anyhow::Result<()> {
    let config_path = project_root.join("epic.toml");
    if config_path.exists() {
        bail!(
            "epic.toml already exists at {}. Delete it first to reinitialize.",
            config_path.display()
        );
    }

    eprintln!("Scanning project for build/test/lint configuration...\n");

    let findings = agent.explore_for_init().await?;
    let (steps, declined) = present_and_confirm(findings)?;
    let models = prompt_models()?;
    let limits = prompt_limits()?;

    let config = EpicConfig {
        verification_steps: steps,
        models,
        limits,
        ..EpicConfig::default()
    };

    let mut toml_str = toml::to_string_pretty(&config)
        .map_err(|e| anyhow::anyhow!("failed to serialize config: {e}"))?;

    // Append declined steps as comments for reference
    if !declined.is_empty() {
        toml_str.push_str("\n# Declined verification steps (uncomment to enable):\n");
        for step in &declined {
            let cmd = step
                .command
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = write!(
                toml_str,
                "# [[verification]]\n# name = \"{}\"\n# command = [{cmd}]\n# timeout = {}\n#\n",
                step.name, step.timeout,
            );
        }
    }

    // Atomic write: write to .tmp then rename
    let tmp_path = config_path.with_extension("toml.tmp");
    std::fs::write(&tmp_path, &toml_str)?;
    std::fs::rename(&tmp_path, &config_path)?;
    println!("\nWrote {}", config_path.display());

    Ok(())
}

/// Present agent findings and interactively confirm each step.
/// Returns (accepted, declined) verification steps.
fn present_and_confirm(
    findings: InitFindingsWire,
) -> anyhow::Result<(Vec<VerificationStep>, Vec<VerificationStep>)> {
    println!("Detected project type: {}", findings.project_type);

    if let Some(notes) = &findings.notes {
        if !notes.is_empty() {
            println!("Notes: {notes}");
        }
    }

    if findings.steps.is_empty() {
        println!("\nNo verification steps detected.");
        println!("You can add them manually to epic.toml later.");
        return Ok((Vec::new(), Vec::new()));
    }

    println!("\nDetected {} verification step(s):\n", findings.steps.len());

    let mut accepted = Vec::new();
    let mut declined = Vec::new();

    {
        let stdin = io::stdin();
        let mut lines = stdin.lock().lines();

        for step in findings.steps {
            print_step(&step);
            print!("  Accept? [Y/n/edit] ");
            io::stdout().flush()?;

            let response = read_line_checked(&mut lines)?;

            match response.as_str() {
                "n" | "no" => {
                    declined.push(VerificationStep::from(step));
                    println!("  Skipped.\n");
                }
                "e" | "edit" => {
                    if let Some(edited) = edit_step(&step, &mut lines)? {
                        accepted.push(edited);
                        println!("  Updated.\n");
                    } else {
                        declined.push(VerificationStep::from(step));
                        println!("  Skipped.\n");
                    }
                }
                _ => {
                    accepted.push(VerificationStep::from(step));
                    println!("  Added.\n");
                }
            }
        }

        // Offer to add custom steps
        loop {
            print!("Add another step? [y/N] ");
            io::stdout().flush()?;

            let response = read_line_or_eof(&mut lines)?;
            if response != "y" && response != "yes" {
                break;
            }

            if let Some(custom) = prompt_custom_step(&mut lines)? {
                accepted.push(custom);
                println!("  Added.\n");
            }
        }

        drop(lines);
    }

    println!(
        "\n{} verification step(s) configured.",
        accepted.len()
    );
    Ok((accepted, declined))
}

fn print_step(step: &DetectedStepWire) {
    let cmd_str = step.command.join(" ");
    println!("  {} — `{cmd_str}`", step.name);
    if let Some(t) = step.timeout {
        println!("    timeout: {t}s");
    }
    println!("    reason: {}", step.rationale);
}

fn edit_step(
    original: &DetectedStepWire,
    lines: &mut impl Iterator<Item = io::Result<String>>,
) -> anyhow::Result<Option<VerificationStep>> {
    let default_cmd = original.command.join(" ");
    let default_timeout = original.timeout.unwrap_or(300);

    print!("  Name [{}]: ", original.name);
    io::stdout().flush()?;
    let name = read_line_or_default(lines, &original.name);

    // Note: whitespace-split does not handle quoted arguments. Commands with spaces
    // in arguments should be edited directly in epic.toml after generation.
    print!("  Command [{default_cmd}]: ");
    io::stdout().flush()?;
    let cmd_input = read_line_or_default(lines, &default_cmd);

    let command: Vec<String> = cmd_input.split_whitespace().map(String::from).collect();

    if command.is_empty() {
        return Ok(None);
    }

    print!("  Timeout [{default_timeout}]: ");
    io::stdout().flush()?;
    let timeout_str = read_line_or_default(lines, &default_timeout.to_string());
    let timeout = timeout_str.parse().unwrap_or(default_timeout);

    Ok(Some(VerificationStep {
        name,
        command,
        timeout,
    }))
}

fn prompt_custom_step(
    lines: &mut impl Iterator<Item = io::Result<String>>,
) -> anyhow::Result<Option<VerificationStep>> {
    print!("  Name: ");
    io::stdout().flush()?;
    let name = read_line(lines);
    if name.is_empty() {
        return Ok(None);
    }

    print!("  Command: ");
    io::stdout().flush()?;
    let cmd_input = read_line(lines);
    let command: Vec<String> = cmd_input.split_whitespace().map(String::from).collect();
    if command.is_empty() {
        return Ok(None);
    }

    print!("  Timeout [300]: ");
    io::stdout().flush()?;
    let timeout_str = read_line_or_default(lines, "300");
    let timeout = timeout_str.parse().unwrap_or(300);

    Ok(Some(VerificationStep {
        name,
        command,
        timeout,
    }))
}

fn prompt_models() -> anyhow::Result<ModelConfig> {
    let defaults = ModelConfig::default();
    println!("\nModel preferences (press Enter to accept defaults):");
    println!("  fast={}, balanced={}, strong={}", defaults.fast, defaults.balanced, defaults.strong);
    print!("  Accept defaults? [Y/n] ");
    io::stdout().flush()?;

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let response = read_line_or_eof(&mut lines)?;

    if response != "n" && response != "no" {
        drop(lines);
        return Ok(defaults);
    }

    print!("  Fast model [{}]: ", defaults.fast);
    io::stdout().flush()?;
    let fast = read_line_or_default(&mut lines, &defaults.fast);
    print!("  Balanced model [{}]: ", defaults.balanced);
    io::stdout().flush()?;
    let balanced = read_line_or_default(&mut lines, &defaults.balanced);
    print!("  Strong model [{}]: ", defaults.strong);
    io::stdout().flush()?;
    let strong = read_line_or_default(&mut lines, &defaults.strong);
    drop(lines);
    Ok(ModelConfig { fast, balanced, strong })
}

fn prompt_limits() -> anyhow::Result<LimitsConfig> {
    let defaults = LimitsConfig::default();
    println!("\nDepth/budget limits (press Enter to accept defaults):");
    println!(
        "  max_depth={}, max_recovery_rounds={}, retry_budget={}, max_total_tasks={}",
        defaults.max_depth, defaults.max_recovery_rounds, defaults.retry_budget, defaults.max_total_tasks
    );
    print!("  Accept defaults? [Y/n] ");
    io::stdout().flush()?;

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let response = read_line_or_eof(&mut lines)?;

    if response != "n" && response != "no" {
        drop(lines);
        return Ok(defaults);
    }

    print!("  Max depth [{}]: ", defaults.max_depth);
    io::stdout().flush()?;
    let max_depth = read_line_or_default(&mut lines, &defaults.max_depth.to_string())
        .parse()
        .unwrap_or(defaults.max_depth);
    print!("  Max recovery rounds [{}]: ", defaults.max_recovery_rounds);
    io::stdout().flush()?;
    let max_recovery_rounds = read_line_or_default(&mut lines, &defaults.max_recovery_rounds.to_string())
        .parse()
        .unwrap_or(defaults.max_recovery_rounds);
    print!("  Retry budget [{}]: ", defaults.retry_budget);
    io::stdout().flush()?;
    let retry_budget = read_line_or_default(&mut lines, &defaults.retry_budget.to_string())
        .parse()
        .unwrap_or(defaults.retry_budget);
    print!("  Max total tasks [{}]: ", defaults.max_total_tasks);
    io::stdout().flush()?;
    let max_total_tasks = read_line_or_default(&mut lines, &defaults.max_total_tasks.to_string())
        .parse()
        .unwrap_or(defaults.max_total_tasks);
    drop(lines);
    Ok(LimitsConfig { max_depth, max_recovery_rounds, retry_budget, max_total_tasks, ..Default::default() })
}

/// Read a line, returning an error on I/O failure or EOF.
fn read_line_checked(
    lines: &mut impl Iterator<Item = io::Result<String>>,
) -> anyhow::Result<String> {
    match lines.next() {
        Some(Ok(line)) => Ok(line.trim().to_lowercase()),
        Some(Err(e)) => bail!("stdin read error: {e}"),
        None => bail!("unexpected end of input"),
    }
}

/// Read a line for optional prompts. Returns empty string on EOF, propagates I/O errors.
fn read_line_or_eof(
    lines: &mut impl Iterator<Item = io::Result<String>>,
) -> anyhow::Result<String> {
    match lines.next() {
        Some(Ok(line)) => Ok(line.trim().to_lowercase()),
        Some(Err(e)) => bail!("stdin read error: {e}"),
        None => Ok(String::new()),
    }
}

fn read_line(lines: &mut impl Iterator<Item = io::Result<String>>) -> String {
    lines
        .next()
        .and_then(Result::ok)
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

fn read_line_or_default(
    lines: &mut impl Iterator<Item = io::Result<String>>,
    default: &str,
) -> String {
    let line = read_line(lines);
    if line.is_empty() { default.to_owned() } else { line }
}
