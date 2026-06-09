pub mod message;
pub mod router;

use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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

            tokio::spawn(async move {
                let (reader, mut writer) = socket.into_split();
                let mut lines = BufReader::new(reader).lines();


                // Channel to send intermediate notifications (like stream tokens) to the client socket
                let (tx, mut rx) = mpsc::channel::<String>(100);

                // Spawn a writer task to forward notifications from the channel to the socket
                let writer_task = tokio::spawn(async move {
                    while let Some(msg) = rx.recv().await {
                        if let Err(e) = writer.write_all(format!("{}\n", msg).as_bytes()).await {
                            error!("Failed to write notification to socket: {}", e);
                            break;
                        }
                    }
                    writer
                });

                while let Ok(Some(line)) = lines.next_line().await {
                    let request: JsonRpcRequest = match serde_json::from_str(&line) {
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
                            let _ = tx.send(serde_json::to_string(&err_response).unwrap()).await;
                            continue;
                        }
                    };

                    let router_clone = router.clone();
                    let tx_clone = tx.clone();

                    tokio::spawn(async move {
                        let response = router_clone.route(request, tx_clone.clone()).await;
                        if let Ok(resp_str) = serde_json::to_string(&response) {
                            let _ = tx_clone.send(resp_str).await;
                        }
                    });
                }

                // Drop sender to signal end of stream
                drop(tx);

                // Wait for the writer task to complete
                let _ = writer_task.await;
                info!("Client disconnected: {}", peer_addr);
            });
        }
    }
}
