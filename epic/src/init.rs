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
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let (steps, declined) = present_and_confirm(findings, &mut lines)?;
    let models = prompt_models(&mut lines)?;
    let limits = prompt_limits(&mut lines)?;
    drop(lines);

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

fn present_and_confirm(
    findings: InitFindingsWire,
    lines: &mut impl Iterator<Item = io::Result<String>>,
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

    println!(
        "\nDetected {} verification step(s):\n",
        findings.steps.len()
    );

    let mut accepted = Vec::new();
    let mut declined = Vec::new();

    for step in findings.steps {
        print_step(&step);
        print!("  Accept? [Y/n/edit] ");
        io::stdout().flush()?;

        let response = read_line_checked(lines)?;

        match response.as_str() {
            "n" | "no" => {
                declined.push(VerificationStep::from(step));
                println!("  Skipped.\n");
            }
            "e" | "edit" => {
                if let Some(edited) = edit_step(&step, lines)? {
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

    loop {
        print!("Add another step? [y/N] ");
        io::stdout().flush()?;

        let response = read_line_or_eof(lines)?;
        if response != "y" && response != "yes" {
            break;
        }

        if let Some(custom) = prompt_custom_step(lines)? {
            accepted.push(custom);
            println!("  Added.\n");
        }
    }

    println!("\n{} verification step(s) configured.", accepted.len());
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
    let name = read_line_or_default(lines, &original.name)?;

    // Note: whitespace-split does not handle quoted arguments. Commands with spaces
    // in arguments should be edited directly in epic.toml after generation.
    print!("  Command [{default_cmd}]: ");
    io::stdout().flush()?;
    let cmd_input = read_line_or_default(lines, &default_cmd)?;

    let command: Vec<String> = cmd_input.split_whitespace().map(String::from).collect();

    if command.is_empty() {
        return Ok(None);
    }

    print!("  Timeout [{default_timeout}]: ");
    io::stdout().flush()?;
    let timeout_str = read_line_or_default(lines, &default_timeout.to_string())?;
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
    let name = read_line(lines)?;
    if name.is_empty() {
        return Ok(None);
    }

    print!("  Command: ");
    io::stdout().flush()?;
    let cmd_input = read_line(lines)?;
    let command: Vec<String> = cmd_input.split_whitespace().map(String::from).collect();
    if command.is_empty() {
        return Ok(None);
    }

    print!("  Timeout [300]: ");
    io::stdout().flush()?;
    let timeout_str = read_line_or_default(lines, "300")?;
    let timeout = timeout_str.parse().unwrap_or(300);

    Ok(Some(VerificationStep {
        name,
        command,
        timeout,
    }))
}

fn prompt_models(
    lines: &mut impl Iterator<Item = io::Result<String>>,
) -> anyhow::Result<ModelConfig> {
    let defaults = ModelConfig::default();
    println!("\nModel preferences (press Enter to accept defaults):");
    println!(
        "  fast={}, balanced={}, strong={}",
        defaults.fast, defaults.balanced, defaults.strong
    );
    print!("  Accept defaults? [Y/n] ");
    io::stdout().flush()?;

    let response = read_line_or_eof(lines)?;

    if response != "n" && response != "no" {
        return Ok(defaults);
    }

    print!("  Fast model [{}]: ", defaults.fast);
    io::stdout().flush()?;
    let fast = read_line_or_default(lines, &defaults.fast)?;
    print!("  Balanced model [{}]: ", defaults.balanced);
    io::stdout().flush()?;
    let balanced = read_line_or_default(lines, &defaults.balanced)?;
    print!("  Strong model [{}]: ", defaults.strong);
    io::stdout().flush()?;
    let strong = read_line_or_default(lines, &defaults.strong)?;
    Ok(ModelConfig {
        fast,
        balanced,
        strong,
    })
}

fn prompt_limits(
    lines: &mut impl Iterator<Item = io::Result<String>>,
) -> anyhow::Result<LimitsConfig> {
    let defaults = LimitsConfig::default();
    println!("\nDepth/budget limits (press Enter to accept defaults):");
    println!(
        "  max_depth={}, max_recovery_rounds={}, retry_budget={}, max_total_tasks={}",
        defaults.max_depth,
        defaults.max_recovery_rounds,
        defaults.retry_budget,
        defaults.max_total_tasks
    );
    print!("  Accept defaults? [Y/n] ");
    io::stdout().flush()?;

    let response = read_line_or_eof(lines)?;

    if response != "n" && response != "no" {
        return Ok(defaults);
    }

    print!("  Max depth [{}]: ", defaults.max_depth);
    io::stdout().flush()?;
    let max_depth = read_line_or_default(lines, &defaults.max_depth.to_string())?
        .parse()
        .unwrap_or(defaults.max_depth);
    print!("  Max recovery rounds [{}]: ", defaults.max_recovery_rounds);
    io::stdout().flush()?;
    let max_recovery_rounds =
        read_line_or_default(lines, &defaults.max_recovery_rounds.to_string())?
            .parse()
            .unwrap_or(defaults.max_recovery_rounds);
    print!("  Retry budget [{}]: ", defaults.retry_budget);
    io::stdout().flush()?;
    let retry_budget = read_line_or_default(lines, &defaults.retry_budget.to_string())?
        .parse()
        .unwrap_or(defaults.retry_budget);
    print!("  Max total tasks [{}]: ", defaults.max_total_tasks);
    io::stdout().flush()?;
    let max_total_tasks = read_line_or_default(lines, &defaults.max_total_tasks.to_string())?
        .parse()
        .unwrap_or(defaults.max_total_tasks);
    Ok(LimitsConfig {
        max_depth,
        max_recovery_rounds,
        retry_budget,
        max_total_tasks,
        ..Default::default()
    })
}

fn read_line_raw(
    lines: &mut impl Iterator<Item = io::Result<String>>,
    bail_on_eof: bool,
    lowercase: bool,
) -> anyhow::Result<String> {
    match lines.next() {
        Some(Ok(line)) => {
            let trimmed = line.trim();
            Ok(if lowercase {
                trimmed.to_lowercase()
            } else {
                trimmed.to_owned()
            })
        }
        Some(Err(e)) => bail!("stdin read error: {e}"),
        None if bail_on_eof => bail!("unexpected end of input"),
        None => Ok(String::new()),
    }
}

fn read_line_checked(
    lines: &mut impl Iterator<Item = io::Result<String>>,
) -> anyhow::Result<String> {
    read_line_raw(lines, true, true)
}

fn read_line_or_eof(
    lines: &mut impl Iterator<Item = io::Result<String>>,
) -> anyhow::Result<String> {
    read_line_raw(lines, false, true)
}

fn read_line(lines: &mut impl Iterator<Item = io::Result<String>>) -> anyhow::Result<String> {
    read_line_raw(lines, false, false)
}

fn read_line_or_default(
    lines: &mut impl Iterator<Item = io::Result<String>>,
    default: &str,
) -> anyhow::Result<String> {
    let line = read_line(lines)?;
    if line.is_empty() {
        Ok(default.to_owned())
    } else {
        Ok(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_lines(inputs: Vec<&str>) -> impl Iterator<Item = io::Result<String>> {
        inputs
            .into_iter()
            .map(|s| Ok(s.to_owned()))
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn sample_findings(steps: Vec<DetectedStepWire>) -> InitFindingsWire {
        InitFindingsWire {
            project_type: "rust".into(),
            steps,
            notes: None,
        }
    }

    fn sample_step(name: &str, cmd: &[&str]) -> DetectedStepWire {
        DetectedStepWire {
            name: name.into(),
            command: cmd.iter().map(|s| (*s).to_owned()).collect(),
            timeout: Some(300),
            rationale: "detected".into(),
        }
    }

    #[test]
    fn present_and_confirm_accept_all() {
        let findings = sample_findings(vec![
            sample_step("build", &["cargo", "build"]),
            sample_step("test", &["cargo", "test"]),
        ]);
        let mut lines = mock_lines(vec!["y", "y", "n"]);
        let (accepted, declined) = present_and_confirm(findings, &mut lines).unwrap();
        assert_eq!(accepted.len(), 2);
        assert!(declined.is_empty());
        assert_eq!(accepted[0].name, "build");
        assert_eq!(accepted[1].name, "test");
    }

    #[test]
    fn present_and_confirm_decline_steps() {
        let findings = sample_findings(vec![
            sample_step("build", &["cargo", "build"]),
            sample_step("test", &["cargo", "test"]),
        ]);
        let mut lines = mock_lines(vec!["n", "n", "n"]);
        let (accepted, declined) = present_and_confirm(findings, &mut lines).unwrap();
        assert!(accepted.is_empty());
        assert_eq!(declined.len(), 2);
    }

    #[test]
    fn present_and_confirm_empty_findings() {
        let findings = sample_findings(vec![]);
        let mut lines = mock_lines(vec![]);
        let (accepted, declined) = present_and_confirm(findings, &mut lines).unwrap();
        assert!(accepted.is_empty());
        assert!(declined.is_empty());
    }

    #[test]
    fn prompt_models_accept_defaults() {
        let mut lines = mock_lines(vec![""]);
        let models = prompt_models(&mut lines).unwrap();
        assert_eq!(models, ModelConfig::default());
    }

    #[test]
    fn prompt_limits_accept_defaults() {
        let mut lines = mock_lines(vec![""]);
        let limits = prompt_limits(&mut lines).unwrap();
        assert_eq!(limits, LimitsConfig::default());
    }

    #[test]
    fn edit_step_changes_values() {
        let step = sample_step("build", &["cargo", "build"]);
        let mut lines = mock_lines(vec!["compile", "cargo build --release", "600"]);
        let edited = edit_step(&step, &mut lines).unwrap().unwrap();
        assert_eq!(edited.name, "compile");
        assert_eq!(edited.command, vec!["cargo", "build", "--release"]);
        assert_eq!(edited.timeout, 600);
    }

    #[test]
    fn edit_step_keeps_defaults() {
        let step = sample_step("build", &["cargo", "build"]);
        let mut lines = mock_lines(vec!["", "", ""]);
        let edited = edit_step(&step, &mut lines).unwrap().unwrap();
        assert_eq!(edited.name, "build");
        assert_eq!(edited.command, vec!["cargo", "build"]);
        assert_eq!(edited.timeout, 300);
    }

    #[test]
    fn prompt_custom_step_creates_step() {
        let mut lines = mock_lines(vec!["lint", "cargo clippy", "120"]);
        let step = prompt_custom_step(&mut lines).unwrap().unwrap();
        assert_eq!(step.name, "lint");
        assert_eq!(step.command, vec!["cargo", "clippy"]);
        assert_eq!(step.timeout, 120);
    }

    #[test]
    fn prompt_custom_step_empty_name_returns_none() {
        let mut lines = mock_lines(vec![""]);
        let step = prompt_custom_step(&mut lines).unwrap();
        assert!(step.is_none());
    }

    #[test]
    fn present_and_confirm_edit_response() {
        let findings = sample_findings(vec![sample_step("build", &["cargo", "build"])]);
        // "edit" for the step, then provide name/command/timeout, then "n" to not add another
        let mut lines = mock_lines(vec!["edit", "compile", "cargo build --release", "600", "n"]);
        let (accepted, declined) = present_and_confirm(findings, &mut lines).unwrap();
        assert_eq!(accepted.len(), 1);
        assert!(declined.is_empty());
        assert_eq!(accepted[0].name, "compile");
        assert_eq!(accepted[0].command, vec!["cargo", "build", "--release"]);
        assert_eq!(accepted[0].timeout, 600);
    }

    #[test]
    fn present_and_confirm_edit_keeps_defaults() {
        let findings = sample_findings(vec![sample_step("build", &["cargo", "build"])]);
        // "e" shorthand triggers edit, then accept all defaults with empty lines
        let mut lines = mock_lines(vec![
            "e", "",  // keep default name
            "",  // keep default command
            "",  // keep default timeout
            "n", // don't add another step
        ]);
        let (accepted, declined) = present_and_confirm(findings, &mut lines).unwrap();
        assert_eq!(accepted.len(), 1);
        assert!(declined.is_empty());
        assert_eq!(accepted[0].name, "build");
        assert_eq!(accepted[0].command, vec!["cargo", "build"]);
        assert_eq!(accepted[0].timeout, 300);
    }

    #[test]
    fn present_and_confirm_add_custom_step() {
        let findings = sample_findings(vec![sample_step("build", &["cargo", "build"])]);
        // Accept the detected step, then add a custom step, then stop
        let mut lines = mock_lines(vec![
            "y",    // accept build
            "y",    // add another step?
            "lint", // custom step name
            "cargo clippy",
            "120",
            "y",   // add another step?
            "fmt", // second custom step name
            "cargo fmt --check",
            "",  // default timeout (300)
            "n", // done adding steps
        ]);
        let (accepted, declined) = present_and_confirm(findings, &mut lines).unwrap();
        assert_eq!(accepted.len(), 3);
        assert!(declined.is_empty());
        assert_eq!(accepted[0].name, "build");
        assert_eq!(accepted[1].name, "lint");
        assert_eq!(accepted[1].command, vec!["cargo", "clippy"]);
        assert_eq!(accepted[1].timeout, 120);
        assert_eq!(accepted[2].name, "fmt");
        assert_eq!(accepted[2].command, vec!["cargo", "fmt", "--check"]);
        assert_eq!(accepted[2].timeout, 300);
    }

    #[test]
    fn prompt_models_custom_values() {
        let mut lines = mock_lines(vec!["n", "gpt-4o-mini", "gpt-4o", "gpt-4-turbo"]);
        let models = prompt_models(&mut lines).unwrap();
        assert_eq!(models.fast, "gpt-4o-mini");
        assert_eq!(models.balanced, "gpt-4o");
        assert_eq!(models.strong, "gpt-4-turbo");
    }

    #[test]
    fn prompt_models_custom_partial_defaults() {
        // Answer "n" to reject defaults, then provide custom fast but accept defaults for balanced/strong
        let defaults = ModelConfig::default();
        let mut lines = mock_lines(vec![
            "no",
            "custom-fast",
            "", // accept default balanced
            "", // accept default strong
        ]);
        let models = prompt_models(&mut lines).unwrap();
        assert_eq!(models.fast, "custom-fast");
        assert_eq!(models.balanced, defaults.balanced);
        assert_eq!(models.strong, defaults.strong);
    }

    #[test]
    fn prompt_limits_custom_values() {
        let mut lines = mock_lines(vec!["n", "10", "5", "8", "50"]);
        let limits = prompt_limits(&mut lines).unwrap();
        assert_eq!(limits.max_depth, 10);
        assert_eq!(limits.max_recovery_rounds, 5);
        assert_eq!(limits.retry_budget, 8);
        assert_eq!(limits.max_total_tasks, 50);
    }

    #[test]
    fn prompt_limits_invalid_numeric_falls_back_to_defaults() {
        let defaults = LimitsConfig::default();
        let mut lines = mock_lines(vec!["n", "not_a_number", "abc", "xyz", "!!!"]);
        let limits = prompt_limits(&mut lines).unwrap();
        assert_eq!(limits.max_depth, defaults.max_depth);
        assert_eq!(limits.max_recovery_rounds, defaults.max_recovery_rounds);
        assert_eq!(limits.retry_budget, defaults.retry_budget);
        assert_eq!(limits.max_total_tasks, defaults.max_total_tasks);
    }

    #[test]
    fn prompt_limits_mixed_valid_and_invalid() {
        let defaults = LimitsConfig::default();
        let mut lines = mock_lines(vec![
            "n", "15",      // valid
            "invalid", // falls back to default
            "7",       // valid
            "invalid", // falls back to default
        ]);
        let limits = prompt_limits(&mut lines).unwrap();
        assert_eq!(limits.max_depth, 15);
        assert_eq!(limits.max_recovery_rounds, defaults.max_recovery_rounds);
        assert_eq!(limits.retry_budget, 7);
        assert_eq!(limits.max_total_tasks, defaults.max_total_tasks);
    }
}
