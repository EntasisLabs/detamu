use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use detamu_model::SourceError;
use detamu_model_code::{FileHistory, RecentFrequency};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::{RepositorySnapshot, git};

const RECENT_WINDOW_SECONDS: i64 = 90 * 24 * 60 * 60;

#[derive(Debug, Default)]
struct HistoryAccumulator {
    created_at: Option<String>,
    last_modified_at: Option<String>,
    last_timestamp: Option<i64>,
    total_interval_days: f64,
    total_commits: u32,
    recent_commits: u32,
    contributors: HashSet<Arc<str>>,
}

impl HistoryAccumulator {
    fn observe(&mut self, timestamp: i64, authored_at: &str, author: Arc<str>, cutoff: i64) {
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
        self.contributors.insert(author);
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

impl super::GitRepositorySource {
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
        Ok(super::cached_inventory(snapshot)
            .await?
            .histories
            .as_ref()
            .clone())
    }
}

pub(super) async fn load_file_histories(
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
    stream_history(
        snapshot,
        snapshot_timestamp.saturating_sub(RECENT_WINDOW_SECONDS),
    )
    .await
}

async fn stream_history(
    snapshot: &RepositorySnapshot,
    recent_cutoff: i64,
) -> Result<HashMap<String, FileHistory>, SourceError> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(&snapshot.root)
        .args([
            "log",
            "--reverse",
            "--topo-order",
            "--format=COMMIT%x00%at%x00%aI%x00%ae%x00",
            "--name-status",
            "-z",
            "-M90",
            snapshot.commit.as_str(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|error| SourceError::Unavailable(format!("run Git: {error}")))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| SourceError::Failed("Git stdout is unavailable".to_owned()))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| SourceError::Failed("Git stderr is unavailable".to_owned()))?;
    let stderr_task = tokio::spawn(async move {
        let mut buffer = Vec::new();
        let _ = stderr.read_to_end(&mut buffer).await;
        buffer
    });

    let mut parser = HistoryParser::new(recent_cutoff);
    let parsed = async {
        let mut buffer = Vec::with_capacity(64 * 1024);
        let mut chunk = vec![0; 64 * 1024];
        loop {
            let read = stdout
                .read(&mut chunk)
                .await
                .map_err(|error| SourceError::Failed(format!("read Git history: {error}")))?;
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            let mut start = 0;
            while let Some(offset) = buffer[start..].iter().position(|byte| *byte == 0) {
                let end = start + offset;
                parser.push(&buffer[start..end])?;
                start = end + 1;
            }
            if start > 0 {
                buffer.drain(..start);
            }
        }
        if !buffer.is_empty() {
            parser.push(&buffer)?;
        }
        Ok::<(), SourceError>(())
    }
    .await;

    let status = child
        .wait()
        .await
        .map_err(|error| SourceError::Failed(format!("wait for Git: {error}")))?;
    let stderr_bytes = stderr_task.await.unwrap_or_default();
    if !status.success() {
        return Err(SourceError::Failed(format!(
            "git log --name-status: {}",
            String::from_utf8_lossy(&stderr_bytes).trim()
        )));
    }
    parsed?;
    Ok(parser.finish())
}

struct HistoryParser {
    histories: HashMap<String, HistoryAccumulator>,
    emails: HashSet<Arc<str>>,
    recent_cutoff: i64,
    timestamp: Option<i64>,
    authored_at: Option<String>,
    author: Option<Arc<str>>,
    step: Step,
}

enum Step {
    Record,
    CommitTime,
    CommitDate,
    CommitAuthor,
    OldPath { copy: bool },
    NewPath { copy: bool, old: String },
    Path,
}

impl HistoryParser {
    fn new(recent_cutoff: i64) -> Self {
        Self {
            histories: HashMap::new(),
            emails: HashSet::new(),
            recent_cutoff,
            timestamp: None,
            authored_at: None,
            author: None,
            step: Step::Record,
        }
    }

    fn push(&mut self, raw: &[u8]) -> Result<(), SourceError> {
        let step = std::mem::replace(&mut self.step, Step::Record);
        match step {
            Step::Record => {
                let token = trim_newlines(raw);
                if token.is_empty() {
                    return Ok(());
                }
                if token == b"COMMIT" {
                    self.step = Step::CommitTime;
                    return Ok(());
                }
                let status = parse_utf8(token, "change status")?;
                if status.starts_with('R') {
                    self.step = Step::OldPath { copy: false };
                } else if status.starts_with('C') {
                    self.step = Step::OldPath { copy: true };
                } else {
                    self.step = Step::Path;
                }
                Ok(())
            }
            Step::CommitTime => {
                self.timestamp = Some(parse_i64(raw, "commit timestamp")?);
                self.step = Step::CommitDate;
                Ok(())
            }
            Step::CommitDate => {
                self.authored_at = Some(parse_text(raw, "authored date")?);
                self.step = Step::CommitAuthor;
                Ok(())
            }
            Step::CommitAuthor => {
                let author = parse_text(raw, "author email")?;
                self.author = Some(self.intern(&author));
                Ok(())
            }
            Step::OldPath { copy } => {
                self.step = Step::NewPath {
                    copy,
                    old: parse_text(raw, "renamed source path")?,
                };
                Ok(())
            }
            Step::NewPath { copy, old } => {
                let new_path = if copy {
                    parse_text(raw, "copied destination path")?
                } else {
                    parse_text(raw, "renamed destination path")?
                };
                if copy {
                    self.observe_path(new_path)
                } else {
                    self.observe_rename(&old, new_path)
                }
            }
            Step::Path => {
                let path = parse_text(raw, "changed path")?;
                self.observe_path(path)
            }
        }
    }

    fn finish(self) -> HashMap<String, FileHistory> {
        self.histories
            .into_iter()
            .filter_map(|(path, accumulator)| accumulator.finish().map(|history| (path, history)))
            .collect()
    }

    fn observe_path(&mut self, path: String) -> Result<(), SourceError> {
        let (timestamp, authored_at, author) = self.current_commit()?;
        self.histories.entry(path).or_default().observe(
            timestamp,
            &authored_at,
            author,
            self.recent_cutoff,
        );
        Ok(())
    }

    fn observe_rename(&mut self, old_path: &str, new_path: String) -> Result<(), SourceError> {
        let (timestamp, authored_at, author) = self.current_commit()?;
        let mut accumulator = self.histories.remove(old_path).unwrap_or_default();
        accumulator.observe(timestamp, &authored_at, author, self.recent_cutoff);
        self.histories.insert(new_path, accumulator);
        Ok(())
    }

    fn current_commit(&self) -> Result<(i64, String, Arc<str>), SourceError> {
        Ok((
            self.timestamp
                .ok_or_else(|| malformed("file change precedes commit"))?,
            self.authored_at
                .clone()
                .ok_or_else(|| malformed("authored date is missing"))?,
            Arc::clone(
                self.author
                    .as_ref()
                    .ok_or_else(|| malformed("author is missing"))?,
            ),
        ))
    }

    fn intern(&mut self, email: &str) -> Arc<str> {
        if let Some(existing) = self.emails.get(email) {
            return Arc::clone(existing);
        }
        let email = Arc::<str>::from(email);
        self.emails.insert(Arc::clone(&email));
        email
    }
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

fn parse_i64(value: &[u8], field: &str) -> Result<i64, SourceError> {
    parse_utf8(value, field)?
        .parse()
        .map_err(|error| SourceError::Failed(format!("invalid {field}: {error}")))
}

fn parse_text(value: &[u8], field: &str) -> Result<String, SourceError> {
    Ok(parse_utf8(value, field)?.to_owned())
}

fn parse_utf8<'a>(value: &'a [u8], field: &str) -> Result<&'a str, SourceError> {
    std::str::from_utf8(value)
        .map_err(|_| SourceError::Failed(format!("Git returned a non-UTF-8 {field}")))
}

fn malformed(message: &str) -> SourceError {
    SourceError::Failed(format!("malformed Git history: {message}"))
}
