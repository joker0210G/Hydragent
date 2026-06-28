use std::net::SocketAddr;
use std::sync::Arc;
use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, State},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use http::StatusCode;
use tokio::net::TcpStream;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use futures_util::{sink::SinkExt, stream::StreamExt};
use tracing::{info, error, warn};
use crate::static_files::Assets;

struct ServerState {
    bus_port: u16,
}

pub async fn start_web_server(port: u16, bus_port: u16) -> anyhow::Result<()> {
    let state = Arc::new(ServerState { bus_port });
    
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .fallback(static_handler)
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    info!("Web Control UI serving on http://{}", addr);
    
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    
    Ok(())
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ServerState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state.bus_port))
}

async fn handle_socket(socket: WebSocket, bus_port: u16) {
    info!("New Web UI WebSocket connection established");
    
    // Connect to the local TCP Event Bus
    let bus_addr = format!("127.0.0.1:{}", bus_port);
    let tcp_stream = match TcpStream::connect(&bus_addr).await {
        Ok(stream) => stream,
        Err(e) => {
            error!("Failed to connect to Event Bus at {}: {}", bus_addr, e);
            return;
        }
    };

    let (mut ws_sender, mut ws_receiver) = socket.split();
    let (tcp_reader, mut tcp_writer) = tcp_stream.into_split();
    let mut tcp_reader = BufReader::new(tcp_reader);

    // Task 1: Forward from WebSocket to TCP Event Bus
    let mut ws_to_tcp = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            match msg {
                Message::Text(text) => {
                    let mut bytes = text.into_bytes();
                    bytes.push(b'\n');
                    if let Err(e) = tcp_writer.write_all(&bytes).await {
                        error!("Failed to write to Event Bus: {}", e);
                        break;
                    }
                    let _ = tcp_writer.flush().await;
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    // Task 2: Forward from TCP Event Bus to WebSocket
    let mut tcp_to_ws = tokio::spawn(async move {
        let mut line = String::new();
        loop {
            line.clear();
            match tcp_reader.read_line(&mut line).await {
                Ok(0) => break, // EOF
                Ok(_) => {
                    if let Err(e) = ws_sender.send(Message::Text(line.clone())).await {
                        error!("Failed to send to WebSocket: {}", e);
                        break;
                    }
                }
                Err(e) => {
                    error!("Error reading from Event Bus: {}", e);
                    break;
                }
            }
        }
    });

    // Wait for either task to finish, then abort the other
    tokio::select! {
        _ = &mut ws_to_tcp => tcp_to_ws.abort(),
        _ = &mut tcp_to_ws => ws_to_tcp.abort(),
    }
    info!("Web UI WebSocket connection closed");
}

async fn static_handler(uri: axum::http::Uri) -> impl IntoResponse {
    let mut path = uri.path().trim_start_matches('/').to_string();
    if path.is_empty() {
        path = "index.html".to_string();
    }

    match Assets::get(&path) {
        Some(content) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            Response::builder()
                .header("content-type", mime.as_ref())
                .status(StatusCode::OK)
                .body(axum::body::Body::from(content.data))
                .unwrap()
        }
        None => {
            // Fallback to index.html for SPA routing
            if let Some(content) = Assets::get("index.html") {
                Response::builder()
                    .header("content-type", "text/html")
                    .status(StatusCode::OK)
                    .body(axum::body::Body::from(content.data))
                    .unwrap()
            } else {
                Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(axum::body::Body::from("Not Found"))
                    .unwrap()
            }
        }
    }
}
