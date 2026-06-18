pub mod message;
pub mod router;

use std::io::{BufRead, BufWriter, Write};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::{info, error, warn};
use crate::router::Router;
use crate::message::{JsonRpcRequest, JsonRpcResponse, JsonRpcError, ERR_PARSE};

pub struct EventBus {
    router: Arc<Router>,
    port: u16,
}

impl EventBus {
    pub fn new(router: Router, port: u16) -> Self {
        Self {
            router: Arc::new(router),
            port,
        }
    }

    pub async fn start(&self) -> anyhow::Result<()> {
        let addr = format!("127.0.0.1:{}", self.port);
        let listener = TcpListener::bind(&addr).await?;
        info!("Event Bus listening on {}", addr);

        loop {
            let (socket, peer_addr) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                    continue;
                }
            };

            info!("New client connected: {}", peer_addr);
            let router = self.router.clone();

            // Convert the tokio TcpStream to a std TcpStream and split it
            // into two halves via `try_clone()`. The clone shares the same
            // underlying OS socket. This sidesteps the Windows tokio bug
            // where `into_split()` / `split()` on a TcpStream cause the read
            // half to observe a spurious EOF after the write half flushes.
            //
            // Each half runs in its own `std::thread` doing blocking I/O.
            // std::net::TcpStream on Windows uses Winsock blocking I/O which
            // is rock-solid for full-duplex sockets.
            let std_socket = match socket.into_std() {
                Ok(s) => s,
                Err(e) => {
                    error!("Failed to convert to std socket: {}", e);
                    continue;
                }
            };
            // tokio leaves the socket in non-blocking mode; flip it back.
            if let Err(e) = std_socket.set_nonblocking(false) {
                error!("Failed to set blocking mode: {}", e);
                continue;
            }
            let read_socket = match std_socket.try_clone() {
                Ok(s) => s,
                Err(e) => {
                    error!("Failed to clone socket: {}", e);
                    continue;
                }
            };
            let write_socket = std_socket;

            // Channel: reader thread → handler  (inbound JSON-RPC lines)
            let (inbound_tx, mut inbound_rx) = mpsc::channel::<String>(100);
            // Channel: handler → writer thread  (outbound JSON-RPC lines)
            let (outbound_tx, mut outbound_rx) = mpsc::channel::<String>(100);

            // ── Reader thread (blocking I/O) ─────────────────────────────
            std::thread::Builder::new()
                .name("hydragent-bus-reader".into())
                .spawn(move || {
                    let mut reader = std::io::BufReader::new(read_socket);
                    loop {
                        let mut buf = String::new();
                        let n = match reader.read_line(&mut buf) {
                            Ok(n) => n,
                            Err(_) => break,
                        };
                        if n == 0 {
                            break;
                        }
                        if inbound_tx.blocking_send(buf).is_err() {
                            break;
                        }
                    }
                })?;

            // ── Writer thread (blocking I/O) ─────────────────────────────
            std::thread::Builder::new()
                .name("hydragent-bus-writer".into())
                .spawn(move || {
                    let mut writer = BufWriter::new(write_socket);
                    while let Some(msg) = outbound_rx.blocking_recv() {
                        let mut bytes = msg.into_bytes();
                        bytes.push(b'\n');
                        if writer.write_all(&bytes).is_err() {
                            break;
                        }
                        if writer.flush().is_err() {
                            break;
                        }
                    }
                })?;

            // ── Handler task (in tokio runtime) ──────────────────────────
            tokio::spawn(async move {
                while let Some(line) = inbound_rx.recv().await {
                    let request: JsonRpcRequest = match serde_json::from_str(line.trim_end()) {
                        Ok(req) => req,
                        Err(e) => {
                            warn!("Failed to parse JSON-RPC request: {}", e);
                            let err_response = JsonRpcResponse {
                                jsonrpc: "2.0".to_string(),
                                result: None,
                                error: Some(JsonRpcError {
                                    code: ERR_PARSE,
                                    message: format!("Parse error: {}", e),
                                    data: None,
                                }),
                                id: "null".to_string(),
                            };
                            let _ = outbound_tx
                                .send(serde_json::to_string(&err_response).unwrap())
                                .await;
                            continue;
                        }
                    };

                    info!(
                        "[BUS] dispatched request id={} method={}",
                        request.id, request.method
                    );

                    let router_clone = router.clone();
                    let outbound_tx_clone = outbound_tx.clone();
                    tokio::spawn(async move {
                        let response = router_clone
                            .route(request, outbound_tx_clone.clone())
                            .await;
                        match serde_json::to_string(&response) {
                            Ok(resp_str) => {
                                if outbound_tx_clone.send(resp_str).await.is_err() {
                                    error!("[BUS] channel send failed");
                                }
                            }
                            Err(e) => error!("[BUS] serialize failed: {}", e),
                        }
                    });
                }
            });
        }
    }
}
