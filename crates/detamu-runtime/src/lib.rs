//! Discovery contract for optional analyzer executables.
//!
//! Detamu discovers and reports runtimes but deliberately does not download,
//! update, or remove them. A host such as Medousa remains responsible for the
//! package lifecycle.

use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::{process::Command, time::timeout};

pub const RUNTIME_SCHEMA_VERSION: u32 = 1;
pub const RUNTIME_DIRECTORY_ENV: &str = "DETAMU_RUNTIME_DIR";
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSpec {
    pub id: String,
    pub executable: String,
    pub environment_override: String,
    pub version_arguments: Vec<String>,
    pub tested_versions: Vec<String>,
    pub optional: bool,
    pub capabilities: Vec<String>,
}

impl RuntimeSpec {
    pub fn lizard() -> Self {
        Self {
            id: "lizard".to_owned(),
            executable: "lizard".to_owned(),
            environment_override: "DETAMU_LIZARD".to_owned(),
            version_arguments: vec!["--version".to_owned()],
            tested_versions: vec!["1.23.0".to_owned()],
            optional: true,
            capabilities: vec!["symbols".to_owned(), "metrics".to_owned()],
        }
    }

    pub fn rust_analyzer() -> Self {
        Self {
            id: "rust-analyzer".to_owned(),
            executable: "rust-analyzer".to_owned(),
            environment_override: "DETAMU_RUST_ANALYZER".to_owned(),
            version_arguments: vec!["--version".to_owned()],
            tested_versions: Vec::new(),
            optional: true,
            capabilities: vec!["references".to_owned(), "calls".to_owned()],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSource {
    Environment,
    EngineSibling,
    ManagedDirectory,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub spec: RuntimeSpec,
    pub available: bool,
    pub executable: PathBuf,
    pub source: Option<RuntimeSource>,
    pub version: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeInventory {
    pub schema_version: u32,
    pub runtime_directory_environment: String,
    pub runtimes: Vec<RuntimeStatus>,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeResolver {
    engine_directory: Option<PathBuf>,
    managed_directory: Option<PathBuf>,
}

impl RuntimeResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_environment() -> Self {
        let engine_directory = std::env::current_exe()
            .ok()
            .and_then(|executable| executable.parent().map(Path::to_owned));
        Self {
            engine_directory,
            managed_directory: std::env::var_os(RUNTIME_DIRECTORY_ENV).map(PathBuf::from),
        }
    }

    #[must_use]
    pub fn with_engine_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.engine_directory = Some(directory.into());
        self
    }

    #[must_use]
    pub fn with_managed_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.managed_directory = Some(directory.into());
        self
    }

    pub async fn resolve(&self, spec: &RuntimeSpec) -> RuntimeStatus {
        if let Some(executable) = std::env::var_os(&spec.environment_override) {
            return probe(spec, PathBuf::from(executable), RuntimeSource::Environment).await;
        }

        let mut candidates = Vec::new();
        if let Some(directory) = &self.engine_directory {
            candidates.push((
                directory.join(executable_filename(&spec.executable)),
                RuntimeSource::EngineSibling,
            ));
        }
        if let Some(directory) = &self.managed_directory {
            candidates.push((
                directory
                    .join("bin")
                    .join(executable_filename(&spec.executable)),
                RuntimeSource::ManagedDirectory,
            ));
            candidates.push((
                directory.join(executable_filename(&spec.executable)),
                RuntimeSource::ManagedDirectory,
            ));
        }
        candidates.push((PathBuf::from(&spec.executable), RuntimeSource::Path));

        let mut seen = BTreeSet::new();
        let mut failures = Vec::new();
        for (candidate, source) in candidates {
            if !seen.insert(candidate.clone()) {
                continue;
            }
            let status = probe(spec, candidate, source).await;
            if status.available {
                return status;
            }
            if let Some(detail) = status.detail {
                failures.push(detail);
            }
        }

        RuntimeStatus {
            spec: spec.clone(),
            available: false,
            executable: PathBuf::from(&spec.executable),
            source: None,
            version: None,
            detail: Some(failures.join("; ")),
        }
    }

    pub async fn inventory(&self, specs: &[RuntimeSpec]) -> RuntimeInventory {
        let mut runtimes = Vec::with_capacity(specs.len());
        for spec in specs {
            runtimes.push(self.resolve(spec).await);
        }
        RuntimeInventory {
            schema_version: RUNTIME_SCHEMA_VERSION,
            runtime_directory_environment: RUNTIME_DIRECTORY_ENV.to_owned(),
            runtimes,
        }
    }
}

async fn probe(spec: &RuntimeSpec, executable: PathBuf, source: RuntimeSource) -> RuntimeStatus {
    let output = timeout(
        PROBE_TIMEOUT,
        Command::new(&executable)
            .args(&spec.version_arguments)
            .output(),
    )
    .await;
    match output {
        Ok(Ok(output)) if output.status.success() => RuntimeStatus {
            spec: spec.clone(),
            available: true,
            executable,
            source: Some(source),
            version: output_version(&output.stdout, &output.stderr),
            detail: None,
        },
        Ok(Ok(output)) => RuntimeStatus {
            spec: spec.clone(),
            available: false,
            executable: executable.clone(),
            source: Some(source),
            version: None,
            detail: Some(format!(
                "{} exited with {}",
                executable.display(),
                output.status
            )),
        },
        Ok(Err(error)) => RuntimeStatus {
            spec: spec.clone(),
            available: false,
            executable: executable.clone(),
            source: Some(source),
            version: None,
            detail: Some(format!("{}: {error}", executable.display())),
        },
        Err(_) => RuntimeStatus {
            spec: spec.clone(),
            available: false,
            executable: executable.clone(),
            source: Some(source),
            version: None,
            detail: Some(format!(
                "{} version probe timed out after {} seconds",
                executable.display(),
                PROBE_TIMEOUT.as_secs()
            )),
        },
    }
}

fn output_version(stdout: &[u8], stderr: &[u8]) -> Option<String> {
    let output = if stdout.is_empty() { stderr } else { stdout };
    String::from_utf8_lossy(output)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
}

fn executable_filename(executable: &str) -> OsString {
    if cfg!(windows) && Path::new(executable).extension().is_none() {
        OsString::from(format!("{executable}.exe"))
    } else {
        OsString::from(executable)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn fixture_spec() -> RuntimeSpec {
        RuntimeSpec {
            id: "fixture".to_owned(),
            executable: "detamu-runtime-fixture".to_owned(),
            environment_override: "DETAMU_TEST_RUNTIME_OVERRIDE_UNSET".to_owned(),
            version_arguments: vec!["--version".to_owned()],
            tested_versions: vec!["fixture".to_owned()],
            optional: true,
            capabilities: vec!["testing".to_owned()],
        }
    }

    async fn write_executable(path: &Path, version: &str) {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.expect("create bin");
        }
        tokio::fs::write(path, format!("#!/bin/sh\necho '{version}'\n"))
            .await
            .expect("write executable");
        let mut permissions = tokio::fs::metadata(path)
            .await
            .expect("executable metadata")
            .permissions();
        permissions.set_mode(0o755);
        tokio::fs::set_permissions(path, permissions)
            .await
            .expect("make executable");
    }

    #[tokio::test]
    async fn engine_sibling_precedes_managed_directory() {
        let temporary = tempfile_directory("precedence");
        let engine = temporary.join("engine");
        let managed = temporary.join("managed");
        write_executable(&engine.join("detamu-runtime-fixture"), "sibling 1").await;
        write_executable(&managed.join("bin/detamu-runtime-fixture"), "managed 2").await;
        let status = RuntimeResolver::new()
            .with_engine_directory(&engine)
            .with_managed_directory(&managed)
            .resolve(&fixture_spec())
            .await;
        assert!(status.available);
        assert_eq!(status.source, Some(RuntimeSource::EngineSibling));
        assert_eq!(status.version.as_deref(), Some("sibling 1"));
        tokio::fs::remove_dir_all(temporary).await.expect("cleanup");
    }

    #[tokio::test]
    async fn managed_directory_supports_data_dir_bin_layout() {
        let temporary = tempfile_directory("managed");
        write_executable(
            &temporary.join("bin/detamu-runtime-fixture"),
            "managed 1.2.3",
        )
        .await;
        let status = RuntimeResolver::new()
            .with_managed_directory(&temporary)
            .resolve(&fixture_spec())
            .await;
        assert!(status.available);
        assert_eq!(status.source, Some(RuntimeSource::ManagedDirectory));
        assert_eq!(status.version.as_deref(), Some("managed 1.2.3"));
        tokio::fs::remove_dir_all(temporary).await.expect("cleanup");
    }

    fn tempfile_directory(label: &str) -> PathBuf {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "detamu-runtime-{label}-{}-{sequence}",
            std::process::id()
        ))
    }
}
