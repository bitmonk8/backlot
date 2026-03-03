use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use xxhash_rust::xxh3::xxh3_128;

use crate::context::Context;
use crate::event::RunSummary;
use crate::model::ReasoningLevel;

/// Fields describing how the run was invoked.
#[derive(Debug, Serialize)]
pub struct Invocation {
    pub config_path: PathBuf,
    pub model: String,
    pub provider: String,
    pub query: String,
    pub raw: bool,
    pub reasoning: Option<ReasoningLevel>,
    pub context_path: Option<PathBuf>,
}

/// Subset of `RunSummary` stored in history.
#[derive(Debug, Serialize)]
struct RunStats {
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: f64,
    iterations: u32,
}

/// One JSON-lines entry in `~/.flick/history.jsonl`.
#[derive(Debug, Serialize)]
struct HistoryEntry {
    timestamp: String,
    invocation: Invocation,
    stats: RunStats,
    context_hash: String,
}

/// Record a completed run to `~/.flick/history.jsonl` and store the
/// context in `~/.flick/contexts/{hash}.json`.
///
/// Failures are non-fatal — callers should warn on stderr and continue.
pub async fn record(
    invocation: Invocation,
    summary: &RunSummary,
    context: &Context,
    flick_dir: &std::path::Path,
) -> Result<(), std::io::Error> {
    use tokio::io::AsyncWriteExt;

    let context_bytes = serde_json::to_vec(context)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let hash = xxh3_128(&context_bytes);
    let hash_hex = format!("{hash:032x}");

    // Write context file (content-addressable dedup)
    let contexts_dir = flick_dir.join("contexts");
    tokio::fs::create_dir_all(&contexts_dir).await?;
    let context_file = contexts_dir.join(format!("{hash_hex}.json"));
    if !tokio::fs::try_exists(&context_file).await.unwrap_or(false) {
        tokio::fs::write(&context_file, &context_bytes).await?;
    }

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
            input_tokens: summary.input_tokens,
            output_tokens: summary.output_tokens,
            cost_usd: summary.cost_usd,
            iterations: summary.iterations,
        },
        context_hash: hash_hex,
    };

    let mut line = serde_json::to_string(&entry)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    line.push('\n');

    // Append to history file
    let history_path = flick_dir.join("history.jsonl");
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&history_path)
        .await?;
    file.write_all(line.as_bytes()).await?;

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
    use crate::context::Context;
    use crate::event::RunSummary;

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

    #[test]
    fn hash_determinism() {
        let mut ctx = Context::default();
        ctx.push_user_text("hello").expect("push");
        let bytes = serde_json::to_vec(&ctx).expect("serialize");
        let h1 = xxh3_128(&bytes);
        let h2 = xxh3_128(&bytes);
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_differs_for_different_contexts() {
        let mut ctx1 = Context::default();
        ctx1.push_user_text("hello").expect("push");
        let mut ctx2 = Context::default();
        ctx2.push_user_text("world").expect("push");
        let h1 = xxh3_128(&serde_json::to_vec(&ctx1).expect("ser"));
        let h2 = xxh3_128(&serde_json::to_vec(&ctx2).expect("ser"));
        assert_ne!(h1, h2);
    }

    fn test_invocation() -> Invocation {
        Invocation {
            config_path: PathBuf::from("test.toml"),
            model: "test-model".into(),
            provider: "test-provider".into(),
            query: "hello".into(),
            raw: false,
            reasoning: None,
            context_path: None,
        }
    }

    fn test_summary() -> RunSummary {
        RunSummary {
            input_tokens: 100,
            output_tokens: 50,
            cost_usd: 0.005,
            iterations: 1,
            context_hash: None,
        }
    }

    #[tokio::test]
    async fn record_creates_context_and_history() {
        let dir = tempfile::tempdir().expect("tempdir");
        let flick_dir = dir.path();

        let mut ctx = Context::default();
        ctx.push_user_text("test query").expect("push");

        record(test_invocation(), &test_summary(), &ctx, flick_dir)
            .await
            .expect("record");

        // Verify history.jsonl exists and has one valid JSON line
        let history = tokio::fs::read_to_string(flick_dir.join("history.jsonl"))
            .await
            .expect("read history");
        let lines: Vec<&str> = history.trim().lines().collect();
        assert_eq!(lines.len(), 1);
        let entry: serde_json::Value =
            serde_json::from_str(lines[0]).expect("parse history line");
        assert!(entry["timestamp"].is_string());
        assert!(entry["context_hash"].is_string());

        // Verify context file exists and round-trips
        let hash = entry["context_hash"].as_str().expect("hash str");
        let ctx_path = flick_dir.join("contexts").join(format!("{hash}.json"));
        assert!(ctx_path.exists());
        let ctx_bytes = tokio::fs::read(&ctx_path).await.expect("read context");
        let restored: Context = serde_json::from_slice(&ctx_bytes).expect("parse context");
        assert_eq!(restored.messages.len(), 1);
    }

    #[tokio::test]
    async fn record_dedup_context_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let flick_dir = dir.path();

        let mut ctx = Context::default();
        ctx.push_user_text("same query").expect("push");

        record(test_invocation(), &test_summary(), &ctx, flick_dir)
            .await
            .expect("record 1");
        record(test_invocation(), &test_summary(), &ctx, flick_dir)
            .await
            .expect("record 2");

        // Two history lines but only one context file
        let history = tokio::fs::read_to_string(flick_dir.join("history.jsonl"))
            .await
            .expect("read history");
        assert_eq!(history.trim().lines().count(), 2);

        let mut entries = tokio::fs::read_dir(flick_dir.join("contexts"))
            .await
            .expect("read_dir");
        let mut count = 0;
        while entries.next_entry().await.expect("next").is_some() {
            count += 1;
        }
        assert_eq!(count, 1);
    }
}
