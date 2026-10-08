//! Stream adapters for a newline-delimited request listener.
//!
//! Register one with [`crate::DetamuBuilder::with_listener`]. Detamu accepts the
//! next session, reads a request line, and writes one response line. TCP and
//! Unix sockets close after that exchange. Standard input stays open and keeps
//! reading until it reaches EOF.

use std::{io, path::Path, sync::Arc};

use async_trait::async_trait;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    net::TcpListener,
};

/// How many requests one accepted session carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    /// One request, one response, then the session closes.
    OneExchange,
    /// Keep reading request lines until the stream reaches EOF.
    UntilEof,
}

/// Failure while accepting a session or copying a request line.
#[derive(Debug, Error)]
pub enum ListenerError {
    /// [`crate::Detamu::serve`] was called before [`crate::DetamuBuilder::with_listener`].
    #[error("no stream listener was registered")]
    Empty,
    /// The adapter could not accept a session or finish a read or write.
    #[error("stream listener failed: {0}")]
    Io(#[from] io::Error),
    /// The task driving a listener stopped unexpectedly.
    #[error("stream listener task stopped")]
    Task,
}

/// One bidirectional byte stream accepted by a [`StreamListener`].
pub struct ListenerSession {
    reader: BufReader<Box<dyn AsyncRead + Send + Unpin>>,
    writer: Box<dyn AsyncWrite + Send + Unpin>,
}

impl ListenerSession {
    /// Wraps a tokio reader and writer as one listener session.
    ///
    /// Use this from a custom [`StreamListener`] when the stream is already a
    /// stdio pipe, a socket, or any other `AsyncRead` + `AsyncWrite` pair.
    pub fn new(
        reader: impl AsyncRead + Send + Unpin + 'static,
        writer: impl AsyncWrite + Send + Unpin + 'static,
    ) -> Self {
        Self {
            reader: BufReader::new(Box::new(reader)),
            writer: Box::new(writer),
        }
    }

    async fn read_line(&mut self) -> Result<Option<String>, ListenerError> {
        let mut line = String::new();
        let read = self.reader.read_line(&mut line).await?;
        if read == 0 { Ok(None) } else { Ok(Some(line)) }
    }

    async fn write_line(&mut self, line: &str) -> Result<(), ListenerError> {
        self.writer.write_all(line.as_bytes()).await?;
        if !line.ends_with('\n') {
            self.writer.write_all(b"\n").await?;
        }
        self.writer.flush().await?;
        Ok(())
    }
}

/// Accepts sessions for [`crate::Detamu::serve`].
///
/// Built-in adapters cover TCP, Unix sockets, and standard input. A host can
/// implement this trait for any other stream it already owns.
#[async_trait]
pub trait StreamListener: Send {
    /// Whether each accepted session carries one exchange or a stream of lines.
    fn session_mode(&self) -> SessionMode {
        SessionMode::OneExchange
    }

    /// Returns the next session, or `None` when the listener is finished.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying accept or connect call fails.
    async fn accept(&mut self) -> Result<Option<ListenerSession>, ListenerError>;
}

/// Handles one request line and returns the response line, without the trailing newline.
#[async_trait]
pub trait LineHandler: Send + Sync {
    /// Builds the response for one trimmed request line.
    async fn handle_line(&self, line: String) -> String;
}

/// Adapts a function into a [`LineHandler`].
pub struct ClosureHandler<F> {
    function: F,
}

impl<F> ClosureHandler<F> {
    /// Wraps `function` so [`crate::Detamu::serve`] can call it for each line.
    pub fn new(function: F) -> Self {
        Self { function }
    }
}

#[async_trait]
impl<F, Fut> LineHandler for ClosureHandler<F>
where
    F: Fn(String) -> Fut + Send + Sync,
    Fut: Future<Output = String> + Send,
{
    async fn handle_line(&self, line: String) -> String {
        (self.function)(line).await
    }
}

/// TCP adapter. Each connection carries one request and one response.
pub struct TcpListenerAdapter {
    listener: TcpListener,
}

impl TcpListenerAdapter {
    /// Binds a TCP listener. `127.0.0.1:9339` matches the engine's default port.
    ///
    /// # Errors
    ///
    /// Returns an error when the address cannot be bound.
    pub async fn bind(address: impl tokio::net::ToSocketAddrs) -> Result<Self, ListenerError> {
        Ok(Self {
            listener: TcpListener::bind(address).await?,
        })
    }

    /// Returns the address the listener actually bound.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket has no local address.
    pub fn local_addr(&self) -> Result<std::net::SocketAddr, ListenerError> {
        Ok(self.listener.local_addr()?)
    }
}

#[async_trait]
impl StreamListener for TcpListenerAdapter {
    async fn accept(&mut self) -> Result<Option<ListenerSession>, ListenerError> {
        let (socket, _) = self.listener.accept().await?;
        Ok(Some(split_stream(socket)))
    }
}

/// Standard-input adapter. The process reads request lines until stdin closes.
pub struct StdioListenerAdapter {
    open: bool,
}

impl StdioListenerAdapter {
    /// Listens on the current process standard input and standard output.
    #[must_use]
    pub fn new() -> Self {
        Self { open: true }
    }
}

impl Default for StdioListenerAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StreamListener for StdioListenerAdapter {
    fn session_mode(&self) -> SessionMode {
        SessionMode::UntilEof
    }

    async fn accept(&mut self) -> Result<Option<ListenerSession>, ListenerError> {
        if !self.open {
            return Ok(None);
        }
        self.open = false;
        Ok(Some(ListenerSession::new(
            tokio::io::stdin(),
            tokio::io::stdout(),
        )))
    }
}

/// Unix-domain socket adapter. Each connection carries one request and one response.
#[cfg(unix)]
pub struct UnixListenerAdapter {
    listener: tokio::net::UnixListener,
}

#[cfg(unix)]
impl UnixListenerAdapter {
    /// Binds a Unix-domain socket at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error when the path cannot be bound.
    pub fn bind(path: impl AsRef<Path>) -> Result<Self, ListenerError> {
        Ok(Self {
            listener: tokio::net::UnixListener::bind(path)?,
        })
    }
}

#[cfg(unix)]
#[async_trait]
impl StreamListener for UnixListenerAdapter {
    async fn accept(&mut self) -> Result<Option<ListenerSession>, ListenerError> {
        let (socket, _) = self.listener.accept().await?;
        Ok(Some(split_stream(socket)))
    }
}

fn split_stream<S>(stream: S) -> ListenerSession
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (reader, writer) = tokio::io::split(stream);
    ListenerSession::new(reader, writer)
}

pub(crate) async fn serve_listeners(
    mut listeners: Vec<Box<dyn StreamListener>>,
    handler: Arc<dyn LineHandler>,
) -> Result<(), ListenerError> {
    if listeners.is_empty() {
        return Err(ListenerError::Empty);
    }
    let mut joins = Vec::with_capacity(listeners.len());
    for mut listener in listeners.drain(..) {
        let handler = Arc::clone(&handler);
        joins.push(tokio::spawn(async move {
            let mode = listener.session_mode();
            loop {
                match listener.accept().await {
                    Ok(None) => return Ok(()),
                    Ok(Some(session)) => {
                        let handler = Arc::clone(&handler);
                        tokio::spawn(async move {
                            if let Err(error) = run_session(session, mode, handler).await {
                                eprintln!("detamu listener session: {error}");
                            }
                        });
                    }
                    Err(error) => return Err(error),
                }
            }
        }));
    }
    for join in joins {
        match join.await {
            Ok(result) => result?,
            Err(_) => return Err(ListenerError::Task),
        }
    }
    Ok(())
}

async fn run_session(
    mut session: ListenerSession,
    mode: SessionMode,
    handler: Arc<dyn LineHandler>,
) -> Result<(), ListenerError> {
    loop {
        let Some(line) = session.read_line().await? else {
            return Ok(());
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let response = handler.handle_line(line.to_owned()).await;
        session.write_line(&response).await?;
        if mode == SessionMode::OneExchange {
            session.writer.shutdown().await?;
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };

    use super::*;

    struct OnceListener {
        session: Option<ListenerSession>,
        mode: SessionMode,
    }

    #[async_trait]
    impl StreamListener for OnceListener {
        fn session_mode(&self) -> SessionMode {
            self.mode
        }

        async fn accept(&mut self) -> Result<Option<ListenerSession>, ListenerError> {
            Ok(self.session.take())
        }
    }

    #[tokio::test]
    async fn custom_adapter_receives_one_response() {
        let (client, server) = tokio::io::duplex(1024);
        let (server_read, server_write) = tokio::io::split(server);
        let listener = OnceListener {
            session: Some(ListenerSession::new(server_read, server_write)),
            mode: SessionMode::OneExchange,
        };
        let serve = tokio::spawn(serve_listeners(
            vec![Box::new(listener)],
            Arc::new(ClosureHandler::new(
                |line| async move { format!("echo:{line}") },
            )),
        ));
        let (mut client_read, mut client_write) = tokio::io::split(client);
        client_write.write_all(b"hello\n").await.expect("write");
        let mut response = String::new();
        let mut byte = [0; 1];
        while client_read.read(&mut byte).await.expect("read") == 1 {
            response.push(byte[0] as char);
            if response.ends_with('\n') {
                break;
            }
        }
        assert_eq!(response, "echo:hello\n");
        serve.await.expect("join").expect("serve");
    }

    #[tokio::test]
    async fn tcp_adapter_closes_after_one_exchange() {
        let listener = TcpListenerAdapter::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let serve = tokio::spawn(serve_listeners(
            vec![Box::new(listener)],
            Arc::new(ClosureHandler::new(
                |line| async move { format!("echo:{line}") },
            )),
        ));
        let mut client = TcpStream::connect(address).await.expect("connect");
        client.write_all(b"ping\n").await.expect("write");
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.expect("read");
        assert_eq!(response, b"echo:ping\n");
        serve.abort();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_adapter_closes_after_one_exchange() {
        let path =
            std::env::temp_dir().join(format!("detamu-listener-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListenerAdapter::bind(&path).expect("bind");
        let serve = tokio::spawn(serve_listeners(
            vec![Box::new(listener)],
            Arc::new(ClosureHandler::new(
                |line| async move { format!("echo:{line}") },
            )),
        ));
        let mut client = tokio::net::UnixStream::connect(&path)
            .await
            .expect("connect");
        client.write_all(b"ping\n").await.expect("write");
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.expect("read");
        assert_eq!(response, b"echo:ping\n");
        serve.abort();
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn missing_listener_is_an_error() {
        let error = serve_listeners(
            Vec::new(),
            Arc::new(ClosureHandler::new(|line| async move { line })),
        )
        .await
        .expect_err("empty");
        assert!(matches!(error, ListenerError::Empty));
    }
}
