use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use detamu_model::SourceError;
use detamu_model_code::{FileHistory, RecentFrequency};

use super::{GitRepositorySource, RepositorySnapshot, git};

const RECENT_WINDOW_SECONDS: i64 = 90 * 24 * 60 * 60;

#[derive(Debug, Default)]
struct HistoryAccumulator {
    created_at: Option<String>,
    last_modified_at: Option<String>,
    last_timestamp: Option<i64>,
    total_interval_days: f64,
    total_commits: u32,
    recent_commits: u32,
    contributors: HashSet<String>,
}

impl HistoryAccumulator {
    fn observe(&mut self, timestamp: i64, authored_at: &str, author: &str, cutoff: i64) {
        self.created_at
            .get_or_insert_with(|| authored_at.to_owned());
        self.last_modified_at = Some(authored_at.to_owned());
        if let Some(previous) = self.last_timestamp {
            let seconds =
                u64::try_from(timestamp.saturating_sub(previous).max(0)).unwrap_or(u64::MAX);
            self.total_interval_days += Duration::from_secs(seconds).as_secs_f64() / 86_400.0;
        }
        self.last_timestamp = Some(timestamp);
        self.total_commits = self.total_commits.saturating_add(1);
        if timestamp >= cutoff {
            self.recent_commits = self.recent_commits.saturating_add(1);
        }
        self.contributors.insert(author.to_owned());
    }

    fn finish(self) -> Option<FileHistory> {
        let intervals = self.total_commits.saturating_sub(1);
        let average_days_between_changes = if intervals == 0 {
            0.0
        } else {
            self.total_interval_days / f64::from(intervals)
        };
        Some(FileHistory {
            created_at: self.created_at?,
            last_modified_at: self.last_modified_at?,
            total_commits: self.total_commits,
            contributors: u32::try_from(self.contributors.len()).unwrap_or(u32::MAX),
            average_days_between_changes,
            recent_commits: self.recent_commits,
            recent_frequency: RecentFrequency::from_recent_commits(self.recent_commits),
        })
    }
}

impl GitRepositorySource {
    /// Extracts per-file Git history in one rename-aware traversal.
    ///
    /// The 90-day recent-activity window is anchored to the requested snapshot,
    /// not the machine's wall clock.
    ///
    /// # Errors
    ///
    /// Returns an error when Git history cannot be read or parsed.
    pub async fn file_histories(
        snapshot: &RepositorySnapshot,
    ) -> Result<HashMap<String, FileHistory>, SourceError> {
        let snapshot_timestamp = git(
            &snapshot.root,
            &["show", "-s", "--format=%at", snapshot.commit.as_str()],
        )
        .await?;
        let snapshot_timestamp = std::str::from_utf8(&snapshot_timestamp.stdout)
            .map_err(|_| SourceError::Failed("Git returned a non-UTF-8 timestamp".to_owned()))?
            .trim()
            .parse::<i64>()
            .map_err(|error| SourceError::Failed(format!("invalid snapshot timestamp: {error}")))?;
        let output = git(
            &snapshot.root,
            &[
                "log",
                "--reverse",
                "--topo-order",
                "--format=COMMIT%x00%at%x00%aI%x00%ae%x00",
                "--name-status",
                "-z",
                "-M90",
                snapshot.commit.as_str(),
            ],
        )
        .await?;
        parse_history(
            &output.stdout,
            snapshot_timestamp.saturating_sub(RECENT_WINDOW_SECONDS),
        )
    }
}

fn parse_history(
    bytes: &[u8],
    recent_cutoff: i64,
) -> Result<HashMap<String, FileHistory>, SourceError> {
    let tokens = bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut histories = HashMap::<String, HistoryAccumulator>::new();
    let mut timestamp = None;
    let mut authored_at = None::<String>;
    let mut author = None::<String>;
    let mut index = 0;

    while index < tokens.len() {
        let token = trim_newlines(tokens[index]);
        if token.is_empty() {
            index += 1;
            continue;
        }
        if token == b"COMMIT" {
            timestamp = Some(parse_i64(tokens.get(index + 1), "commit timestamp")?);
            authored_at = Some(parse_text(tokens.get(index + 2), "authored date")?);
            author = Some(parse_text(tokens.get(index + 3), "author email")?);
            index += 4;
            continue;
        }

        let status = parse_utf8(token, "change status")?;
        let current_timestamp =
            timestamp.ok_or_else(|| malformed("file change precedes commit"))?;
        let current_authored_at = authored_at
            .as_deref()
            .ok_or_else(|| malformed("authored date is missing"))?;
        let current_author = author
            .as_deref()
            .ok_or_else(|| malformed("author is missing"))?;

        if status.starts_with('R') {
            let old_path = parse_text(tokens.get(index + 1), "renamed source path")?;
            let new_path = parse_text(tokens.get(index + 2), "renamed destination path")?;
            let mut accumulator = histories.remove(&old_path).unwrap_or_default();
            accumulator.observe(
                current_timestamp,
                current_authored_at,
                current_author,
                recent_cutoff,
            );
            histories.insert(new_path, accumulator);
            index += 3;
        } else if status.starts_with('C') {
            let new_path = parse_text(tokens.get(index + 2), "copied destination path")?;
            histories.entry(new_path).or_default().observe(
                current_timestamp,
                current_authored_at,
                current_author,
                recent_cutoff,
            );
            index += 3;
        } else {
            let path = parse_text(tokens.get(index + 1), "changed path")?;
            histories.entry(path).or_default().observe(
                current_timestamp,
                current_authored_at,
                current_author,
                recent_cutoff,
            );
            index += 2;
        }
    }

    Ok(histories
        .into_iter()
        .filter_map(|(path, accumulator)| accumulator.finish().map(|history| (path, history)))
        .collect())
}

fn trim_newlines(mut value: &[u8]) -> &[u8] {
    while value
        .first()
        .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
    {
        value = &value[1..];
    }
    value
}

fn parse_i64(value: Option<&&[u8]>, field: &str) -> Result<i64, SourceError> {
    parse_utf8(value.copied().unwrap_or_default(), field)?
        .parse()
        .map_err(|error| SourceError::Failed(format!("invalid {field}: {error}")))
}

fn parse_text(value: Option<&&[u8]>, field: &str) -> Result<String, SourceError> {
    Ok(parse_utf8(value.copied().unwrap_or_default(), field)?.to_owned())
}

fn parse_utf8<'a>(value: &'a [u8], field: &str) -> Result<&'a str, SourceError> {
    std::str::from_utf8(value)
        .map_err(|_| SourceError::Failed(format!("Git returned a non-UTF-8 {field}")))
}

fn malformed(message: &str) -> SourceError {
    SourceError::Failed(format!("malformed Git history: {message}"))
}
