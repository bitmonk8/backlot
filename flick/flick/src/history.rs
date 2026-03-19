use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::model::ReasoningLevel;
use crate::result::UsageSummary;

/// Fields describing how the run was invoked.
#[derive(Debug, Serialize)]
pub struct Invocation {
    pub config_path: PathBuf,
    pub model: String,
    pub provider: String,
    pub query: String,
    pub reasoning: Option<ReasoningLevel>,
    pub resume_hash: Option<String>,
}

/// Token/cost stats stored in history.
#[derive(Debug, Serialize)]
struct RunStats {
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
}

/// One JSON-lines entry in `~/.flick/history.jsonl`.
#[derive(Debug, Serialize)]
struct HistoryEntry {
    timestamp: String,
    invocation: Invocation,
    stats: RunStats,
    context_hash: String,
}

/// Record a completed run to `~/.flick/history.jsonl`.
///
/// The caller is responsible for writing the context file to
/// `~/.flick/contexts/{hash}.json` and passing the pre-computed hash.
///
/// Failures are non-fatal — callers should warn on stderr and continue.
pub async fn record(
    invocation: Invocation,
    usage: &UsageSummary,
    context_hash: &str,
    flick_dir: &std::path::Path,
) -> Result<(), std::io::Error> {
    use tokio::io::AsyncWriteExt;

    // Build history entry
    let timestamp = epoch_to_utc(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    );
    let entry = HistoryEntry {
        timestamp,
        invocation,
        stats: RunStats {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cost_usd: usage.cost_usd,
        },
        context_hash: context_hash.to_string(),
    };

    let mut line =
        serde_json::to_string(&entry).map_err(|e| std::io::Error::other(e.to_string()))?;
    line.push('\n');

    // Append to history file
    let history_path = flick_dir.join("history.jsonl");
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&history_path)
        .await?;
    file.write_all(line.as_bytes()).await?;
    file.flush().await?;

    Ok(())
}

/// Convert epoch seconds to an RFC 3339 UTC timestamp string.
///
/// Manual calendar math avoids adding a `chrono` dependency.
fn epoch_to_utc(epoch_secs: u64) -> String {
    let days = epoch_secs / 86400;
    let day_secs = epoch_secs % 86400;
    let hour = day_secs / 3600;
    let minute = (day_secs % 3600) / 60;
    let second = day_secs % 60;

    // Days since 1970-01-01 → (year, month, day) using a civil calendar algorithm.
    // Based on Howard Hinnant's algorithm: http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::result::UsageSummary;

    #[test]
    fn epoch_to_utc_unix_epoch() {
        assert_eq!(epoch_to_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn epoch_to_utc_known_timestamp() {
        // 2024-01-15T12:30:45Z = 1705321845
        assert_eq!(epoch_to_utc(1_705_321_845), "2024-01-15T12:30:45Z");
    }

    #[test]
    fn epoch_to_utc_y2k() {
        // 2000-01-01T00:00:00Z = 946684800
        assert_eq!(epoch_to_utc(946_684_800), "2000-01-01T00:00:00Z");
    }

    #[test]
    fn epoch_to_utc_leap_year() {
        // 2024-02-29T00:00:00Z = 1709164800
        assert_eq!(epoch_to_utc(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    fn test_invocation() -> Invocation {
        Invocation {
            config_path: PathBuf::from("test.yaml"),
            model: "test-model".into(),
            provider: "test-provider".into(),
            query: "hello".into(),
            reasoning: None,
            resume_hash: None,
        }
    }

    fn test_usage() -> UsageSummary {
        UsageSummary {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cost_usd: 0.005,
        }
    }

    #[tokio::test]
    async fn record_writes_history_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let flick_dir = dir.path();

        let hash = "00112233445566778899aabbccddeeff";
        record(test_invocation(), &test_usage(), hash, flick_dir)
            .await
            .expect("record");

        // Verify history.jsonl exists and has one valid JSON line
        let history = tokio::fs::read_to_string(flick_dir.join("history.jsonl"))
            .await
            .expect("read history");
        let lines: Vec<&str> = history.trim().lines().collect();
        assert_eq!(lines.len(), 1);
        let entry: serde_json::Value = serde_json::from_str(lines[0]).expect("parse history line");
        assert!(entry["timestamp"].is_string());
        assert_eq!(entry["context_hash"].as_str().expect("hash"), hash);
    }

    #[tokio::test]
    async fn record_with_resume_hash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let flick_dir = dir.path();

        let invocation = Invocation {
            config_path: PathBuf::from("test.yaml"),
            model: "test-model".into(),
            provider: "test-provider".into(),
            query: "continue".into(),
            reasoning: None,
            resume_hash: Some("somehash".into()),
        };
        let hash = "aabbccdd";
        record(invocation, &test_usage(), hash, flick_dir)
            .await
            .expect("record");

        let history = tokio::fs::read_to_string(flick_dir.join("history.jsonl"))
            .await
            .expect("read history");
        let line = history.trim();
        assert!(
            line.contains(r#""resume_hash":"somehash""#),
            "history line should contain resume_hash: {line}"
        );
    }

    #[tokio::test]
    async fn record_appends_multiple_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let flick_dir = dir.path();

        let hash = "00112233445566778899aabbccddeeff";
        record(test_invocation(), &test_usage(), hash, flick_dir)
            .await
            .expect("record 1");
        record(test_invocation(), &test_usage(), hash, flick_dir)
            .await
            .expect("record 2");

        let history = tokio::fs::read_to_string(flick_dir.join("history.jsonl"))
            .await
            .expect("read history");
        assert_eq!(history.trim().lines().count(), 2);
    }
}
