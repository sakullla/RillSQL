use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, thiserror::Error)]
pub enum ForwardError {
    #[error("转发器监听失败：{0}")]
    Bind(String),
    #[error("打开 direct-tcpip 失败：{0}")]
    Channel(String),
    #[error("转发中断：{0}")]
    Io(String),
}

pub struct Pipe {
    inner: Pin<Box<dyn IoPipe>>,
}

trait IoPipe: AsyncRead + AsyncWrite + Send {}

impl<T> IoPipe for T where T: AsyncRead + AsyncWrite + Send {}

impl Pipe {
    pub fn new<T>(stream: T) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + 'static,
    {
        Self {
            inner: Box::pin(stream),
        }
    }
}

impl AsyncRead for Pipe {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        self.inner.as_mut().poll_read(cx, buf)
    }
}

impl AsyncWrite for Pipe {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        self.inner.as_mut().poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.inner.as_mut().poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.inner.as_mut().poll_shutdown(cx)
    }
}

#[async_trait::async_trait]
pub trait DirectTcpOpener: Send + Sync {
    async fn open_direct_tcpip(&self, host: &str, port: u16) -> Result<Pipe, ForwardError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRecord {
    pub id: u64,
    pub host: String,
    pub port: u16,
}

pub struct Forwarder {
    listener: TcpListener,
}

impl Forwarder {
    pub async fn bind() -> Result<Self, ForwardError> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|err| ForwardError::Bind(err.to_string()))?;
        Ok(Self { listener })
    }

    pub fn local_port(&self) -> Result<u16, ForwardError> {
        self.listener
            .local_addr()
            .map(|addr| addr.port())
            .map_err(|err| ForwardError::Bind(err.to_string()))
    }

    /// 每接受一条本地连接，就向数据库地址新开一条 direct-tcpip。
    /// `tls_server_name` 与 `bastion_host` 只用于约束调用方不要把它们当成通道目标。
    pub async fn serve_n<O: DirectTcpOpener + 'static>(
        self,
        opener: Arc<O>,
        channel_host: &str,
        channel_port: u16,
        tls_server_name: &str,
        bastion_host: &str,
        accepts: usize,
    ) -> Result<(), ForwardError> {
        let _keep_identity_separate = (tls_server_name, bastion_host);
        for _ in 0..accepts {
            let (stream, _) = self
                .listener
                .accept()
                .await
                .map_err(|err| ForwardError::Io(err.to_string()))?;
            let remote = opener.open_direct_tcpip(channel_host, channel_port).await?;
            tokio::spawn(async move {
                let _ = copy_both(stream, remote).await;
            });
        }
        Ok(())
    }

    /// 持续接受本地连接，直到 `shutdown` 结束。每条 accept 只打开一条到数据库地址的 direct-tcpip。
    pub async fn serve_until<O: DirectTcpOpener + 'static>(
        self,
        opener: Arc<O>,
        channel_host: String,
        channel_port: u16,
        tls_server_name: String,
        bastion_host: String,
        mut shutdown: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<(), ForwardError> {
        let _keep_identity_separate = (tls_server_name, bastion_host);
        loop {
            tokio::select! {
                _ = &mut shutdown => return Ok(()),
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted.map_err(|err| ForwardError::Io(err.to_string()))?;
                    let remote = opener.open_direct_tcpip(&channel_host, channel_port).await?;
                    tokio::spawn(async move {
                        let _ = copy_both(stream, remote).await;
                    });
                }
            }
        }
    }
}

async fn copy_both(mut local: TcpStream, mut remote: Pipe) -> Result<(), ForwardError> {
    let (mut local_read, mut local_write) = local.split();
    let (mut remote_read, mut remote_write) = tokio::io::split(&mut remote);
    let outbound = tokio::io::copy(&mut local_read, &mut remote_write);
    let inbound = tokio::io::copy(&mut remote_read, &mut local_write);
    tokio::try_join!(outbound, inbound).map_err(|err| ForwardError::Io(err.to_string()))?;
    Ok(())
}

#[derive(Clone)]
pub struct RecordingOpener {
    pub opens: Arc<std::sync::Mutex<Vec<OpenRecord>>>,
    next: Arc<std::sync::atomic::AtomicU64>,
}

impl RecordingOpener {
    pub fn new() -> Self {
        Self {
            opens: Arc::new(std::sync::Mutex::new(Vec::new())),
            next: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        }
    }
}

#[async_trait::async_trait]
impl DirectTcpOpener for RecordingOpener {
    async fn open_direct_tcpip(&self, host: &str, port: u16) -> Result<Pipe, ForwardError> {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.opens
            .lock()
            .expect("opens")
            .push(OpenRecord {
                id,
                host: host.to_owned(),
                port,
            });
        let (client, _server) = tokio::io::duplex(64);
        Ok(Pipe::new(client))
    }
}
