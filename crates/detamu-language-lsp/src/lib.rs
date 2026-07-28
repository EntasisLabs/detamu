//! Generic Language Server Protocol process lifecycle for Detamu adapters.

use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use async_trait::async_trait;
use detamu_core::ObservationBatch;
use detamu_model::{
    AnalysisInput, AnalyzerDescriptor, AnalyzerError, AnalyzerExecution, ModelAnalyzer,
};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};

#[derive(Debug, Clone, PartialEq)]
pub struct LspServerConfig {
    pub command: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub root_uri: Option<String>,
    pub initialization_options: Option<Value>,
    pub request_timeout: Duration,
}

impl LspServerConfig {
    pub fn new(command: impl Into<PathBuf>) -> Self {
        Self {
            command: command.into(),
            arguments: Vec::new(),
            working_directory: None,
            root_uri: None,
            initialization_options: None,
            request_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Error)]
pub enum LspError {
    #[error("language server is unavailable: {0}")]
    Unavailable(String),
    #[error("language server protocol error: {0}")]
    Protocol(String),
    #[error("language server request timed out")]
    Timeout,
    #[error("language server returned an error: {0}")]
    Server(String),
}

pub struct LspSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_request_id: u64,
    request_timeout: Duration,
    initialized: bool,
}

impl LspSession {
    /// Starts and initializes one stdio language-server process.
    ///
    /// # Errors
    ///
    /// Returns an unavailable error when the configured binary cannot start,
    /// or a protocol error when initialization fails.
    pub async fn start(config: &LspServerConfig) -> Result<Self, LspError> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(directory) = &config.working_directory {
            command.current_dir(directory);
        }
        let mut child = command
            .spawn()
            .map_err(|error| LspError::Unavailable(error.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| LspError::Protocol("server stdin is unavailable".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| LspError::Protocol("server stdout is unavailable".to_owned()))?;
        let mut session = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_request_id: 1,
            request_timeout: config.request_timeout,
            initialized: false,
        };
        session
            .request(
                "initialize",
                json!({
                    "processId": std::process::id(),
                    "rootUri": config.root_uri,
                    "workspaceFolders": config.root_uri.as_ref().map(|uri| vec![json!({
                        "uri": uri,
                        "name": "detamu-workspace",
                    })]),
                    "capabilities": {
                        "workspace": { "workspaceFolders": true, "symbol": {} },
                        "textDocument": {
                            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                            "references": {},
                            "callHierarchy": {},
                        }
                    },
                    "initializationOptions": config.initialization_options,
                }),
            )
            .await?;
        session.notify("initialized", json!({})).await?;
        session.initialized = true;
        Ok(session)
    }

    /// Sends one JSON-RPC request and waits for its matching response.
    ///
    /// # Errors
    ///
    /// Returns an error on framing, timeout, I/O, or server-reported failure.
    pub async fn request(&mut self, method: &str, parameters: Value) -> Result<Value, LspError> {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": parameters,
        }))
        .await?;
        timeout(self.request_timeout, async {
            loop {
                let message = read_message(&mut self.stdout).await?;
                if message.get("id").and_then(Value::as_u64) != Some(id) {
                    if message.get("method").is_some()
                        && let Some(server_id) = message.get("id").cloned()
                    {
                        self.write_message(&json!({
                            "jsonrpc": "2.0",
                            "id": server_id,
                            "result": null,
                        }))
                        .await?;
                    }
                    continue;
                }
                if let Some(error) = message.get("error") {
                    return Err(LspError::Server(error.to_string()));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
        })
        .await
        .map_err(|_| LspError::Timeout)?
    }

    /// Sends one JSON-RPC notification.
    ///
    /// # Errors
    ///
    /// Returns an error when the framed message cannot be written.
    pub async fn notify(&mut self, method: &str, parameters: Value) -> Result<(), LspError> {
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": parameters,
        }))
        .await
    }

    /// Waits for the next server notification with the requested method.
    ///
    /// # Errors
    ///
    /// Returns an error on framing, timeout, or I/O failure.
    pub async fn wait_for_notification(&mut self, method: &str) -> Result<Value, LspError> {
        timeout(self.request_timeout, async {
            loop {
                let message = read_message(&mut self.stdout).await?;
                if message.get("method").and_then(Value::as_str) == Some(method)
                    && message.get("id").is_none()
                {
                    return Ok(message.get("params").cloned().unwrap_or(Value::Null));
                }
                if message.get("method").is_some()
                    && let Some(server_id) = message.get("id").cloned()
                {
                    self.write_message(&json!({
                        "jsonrpc": "2.0",
                        "id": server_id,
                        "result": null,
                    }))
                    .await?;
                }
            }
        })
        .await
        .map_err(|_| LspError::Timeout)?
    }

    /// Performs the LSP shutdown/exit handshake and waits for the process.
    ///
    /// # Errors
    ///
    /// Returns an error when the handshake or process wait fails.
    pub async fn shutdown(mut self) -> Result<(), LspError> {
        if self.initialized {
            self.request("shutdown", Value::Null).await?;
            self.notify("exit", Value::Null).await?;
        }
        self.child
            .wait()
            .await
            .map_err(|error| LspError::Protocol(format!("wait for language server: {error}")))?;
        Ok(())
    }

    async fn write_message(&mut self, message: &Value) -> Result<(), LspError> {
        let body = serde_json::to_vec(message)
            .map_err(|error| LspError::Protocol(format!("serialize request: {error}")))?;
        self.stdin
            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .await
            .map_err(|error| LspError::Protocol(format!("write request header: {error}")))?;
        self.stdin
            .write_all(&body)
            .await
            .map_err(|error| LspError::Protocol(format!("write request body: {error}")))?;
        self.stdin
            .flush()
            .await
            .map_err(|error| LspError::Protocol(format!("flush request: {error}")))
    }
}

#[async_trait]
pub trait LspAdapter: Send + Sync + 'static {
    fn descriptor(&self) -> AnalyzerDescriptor;

    /// Resolves the language-server process configuration for this snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot cannot be mapped to a server workspace.
    fn server(&self, input: &AnalysisInput) -> Result<LspServerConfig, AnalyzerError>;

    /// Requests and normalizes semantic observations from an initialized server.
    ///
    /// # Errors
    ///
    /// Returns an error when requests fail or responses cannot be normalized.
    async fn analyze(
        &self,
        session: &mut LspSession,
        input: &AnalysisInput,
    ) -> Result<ObservationBatch, AnalyzerError>;
}

pub struct LspAnalyzer<A> {
    adapter: Arc<A>,
}

impl<A> LspAnalyzer<A> {
    pub fn new(adapter: A) -> Self {
        Self {
            adapter: Arc::new(adapter),
        }
    }
}

#[async_trait]
impl<A: LspAdapter> ModelAnalyzer for LspAnalyzer<A> {
    fn descriptor(&self) -> AnalyzerDescriptor {
        let mut descriptor = self.adapter.descriptor();
        descriptor.execution = AnalyzerExecution::Optional;
        descriptor
    }

    async fn analyze(&self, input: &AnalysisInput) -> Result<ObservationBatch, AnalyzerError> {
        let config = self.adapter.server(input)?;
        let mut session = LspSession::start(&config).await.map_err(map_lsp_error)?;
        let result = self.adapter.analyze(&mut session, input).await;
        let shutdown = session.shutdown().await.map_err(map_lsp_error);
        match (result, shutdown) {
            (Ok(batch), Ok(())) => Ok(batch),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }
}

fn map_lsp_error(error: LspError) -> AnalyzerError {
    match error {
        LspError::Unavailable(message) => AnalyzerError::Unavailable(message),
        other => AnalyzerError::Failed(other.to_string()),
    }
}

async fn read_message<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Value, LspError> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let bytes = reader
            .read_line(&mut line)
            .await
            .map_err(|error| LspError::Protocol(format!("read response header: {error}")))?;
        if bytes == 0 {
            return Err(LspError::Protocol(
                "language server closed stdout".to_owned(),
            ));
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line.trim().strip_prefix("Content-Length:").map(str::trim) {
            content_length =
                Some(value.parse::<usize>().map_err(|error| {
                    LspError::Protocol(format!("invalid Content-Length: {error}"))
                })?);
        }
    }
    let length = content_length
        .ok_or_else(|| LspError::Protocol("response lacks Content-Length".to_owned()))?;
    let mut body = vec![0_u8; length];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|error| LspError::Protocol(format!("read response body: {error}")))?;
    serde_json::from_slice(&body)
        .map_err(|error| LspError::Protocol(format!("decode response JSON: {error}")))
}

#[cfg(test)]
mod tests {
    use tokio::io::BufReader;

    use super::*;

    #[tokio::test]
    async fn reads_content_length_framed_messages() {
        let body = br#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#;
        let wire = format!(
            "Content-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        let mut reader = BufReader::new(wire.as_bytes());
        let message = read_message(&mut reader).await.expect("read message");
        assert_eq!(message["id"], 7);
        assert_eq!(message["result"]["ok"], true);
    }
}
