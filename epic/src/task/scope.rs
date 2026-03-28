// Scope circuit breaker: git diff analysis against magnitude estimates.

use super::Magnitude;
use std::path::Path;

const GIT_DIFF_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, PartialEq, Eq)]
pub enum ScopeCheck {
    WithinBounds,
    Exceeded {
        metric: String,
        actual: u64,
        limit: u64,
    },
}

pub async fn git_diff_numstat(project_root: &Path) -> Option<String> {
    let git_future = tokio::process::Command::new("git")
        .args(["diff", "--numstat", "HEAD"])
        .current_dir(project_root)
        .output();

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(GIT_DIFF_TIMEOUT_SECS),
        git_future,
    )
    .await
    .ok()?
    .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        None
    }
}

pub fn evaluate_scope(numstat_output: &str, magnitude: &Magnitude) -> ScopeCheck {
    let mut total_added: u64 = 0;
    let mut total_deleted: u64 = 0;
    let mut total_modified: u64 = 0;

    for line in numstat_output.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        // Binary files show "-" for counts; skip them.
        let Ok(added) = parts[0].parse::<u64>() else {
            continue;
        };
        let Ok(deleted) = parts[1].parse::<u64>() else {
            continue;
        };
        let modified = added.min(deleted);
        total_added += added - modified;
        total_deleted += deleted - modified;
        total_modified += modified;
    }

    let multiplier = 3;
    // Skip dimensions where the estimate is zero — zero means "unconstrained"
    // (the LLM omitted this dimension). Checking 3x0 = 0 would trip on any change.
    if magnitude.max_lines_added > 0 && total_added > magnitude.max_lines_added * multiplier {
        return ScopeCheck::Exceeded {
            metric: "lines_added".into(),
            actual: total_added,
            limit: magnitude.max_lines_added * multiplier,
        };
    }
    if magnitude.max_lines_modified > 0
        && total_modified > magnitude.max_lines_modified * multiplier
    {
        return ScopeCheck::Exceeded {
            metric: "lines_modified".into(),
            actual: total_modified,
            limit: magnitude.max_lines_modified * multiplier,
        };
    }
    if magnitude.max_lines_deleted > 0 && total_deleted > magnitude.max_lines_deleted * multiplier {
        return ScopeCheck::Exceeded {
            metric: "lines_deleted".into(),
            actual: total_deleted,
            limit: magnitude.max_lines_deleted * multiplier,
        };
    }

    ScopeCheck::WithinBounds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluate_scope_within_bounds() {
        let output = "10\t5\tfile1.rs\n3\t0\tfile2.rs";
        let magnitude = Magnitude {
            max_lines_added: 10,
            max_lines_modified: 5,
            max_lines_deleted: 5,
        };
        assert_eq!(evaluate_scope(output, &magnitude), ScopeCheck::WithinBounds);
    }

    #[test]
    fn evaluate_scope_exceeded() {
        let output = "100\t0\tfile1.rs";
        let magnitude = Magnitude {
            max_lines_added: 10,
            max_lines_modified: 5,
            max_lines_deleted: 5,
        };
        let result = evaluate_scope(output, &magnitude);
        match result {
            ScopeCheck::Exceeded {
                metric,
                actual,
                limit,
            } => {
                assert_eq!(metric, "lines_added");
                assert_eq!(actual, 100);
                assert_eq!(limit, 30);
            }
            ScopeCheck::WithinBounds => panic!("expected Exceeded"),
        }
    }

    #[test]
    fn evaluate_scope_binary_files_skipped() {
        let output = "-\t-\tbinary.png\n5\t2\tcode.rs";
        let magnitude = Magnitude {
            max_lines_added: 10,
            max_lines_modified: 5,
            max_lines_deleted: 5,
        };
        assert_eq!(evaluate_scope(output, &magnitude), ScopeCheck::WithinBounds);
    }

    #[test]
    fn evaluate_scope_empty_output() {
        let magnitude = Magnitude {
            max_lines_added: 10,
            max_lines_modified: 5,
            max_lines_deleted: 5,
        };
        assert_eq!(evaluate_scope("", &magnitude), ScopeCheck::WithinBounds);
    }

    #[test]
    fn evaluate_scope_zero_estimate_unconstrained() {
        let output = "1000\t0\tbig.rs";
        let magnitude = Magnitude {
            max_lines_added: 0,
            max_lines_modified: 5,
            max_lines_deleted: 5,
        };
        assert_eq!(evaluate_scope(output, &magnitude), ScopeCheck::WithinBounds);
    }
}
