use dioxus::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use futures_util::StreamExt;
use gloo_net::websocket::futures::WebSocket;
use gloo_net::websocket::Message;

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Chat,
    Library,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ChatMessage {
    sender: String,
    content: String,
    is_tool: bool,
    tool_status: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LibraryNode {
    id: String,
    title: String,
    node_type: String, // "shelf" | "book" | "page"
    parent_id: Option<String>,
}

fn main() {
    launch(app);
}

fn app() -> Element {
    let mut current_tab = use_signal(|| Tab::Chat);
    let mut input_text = use_signal(String::new);
    let mut messages = use_signal(Vec::<ChatMessage>::new);
    let mut library_nodes = use_signal(Vec::<LibraryNode>::new);
    let mut connection_status = use_signal(|| "Disconnected".to_string());
    let mut ws_tx = use_signal(|| None::<futures_util::stream::SplitSink<WebSocket, Message>>);

    // Initialize WebSocket connection
    let _ws_future = use_resource(move || async move {
        let window = web_sys::window().unwrap();
        let location = window.location();
        let host = location.host().unwrap();
        let protocol = if location.protocol().unwrap() == "https:" { "wss:" } else { "ws:" };
        let ws_url = format!("{}//{}/ws", protocol, host);

        connection_status.set("Connecting...".to_string());
        match WebSocket::open(&ws_url) {
            Ok(ws) => {
                connection_status.set("Connected".to_string());
                let (write, mut read) = ws.split();
                ws_tx.set(Some(write));

                // Spawn read loop
                spawn(async move {
                    while let Some(msg) = read.next().await {
                        if let Ok(Message::Text(text)) = msg {
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                                // Handle incoming message
                                if let Some(msg_type) = val.get("type").and_then(|t| t.as_str()) {
                                    match msg_type {
                                        "token" => {
                                            if let Some(token) = val.get("token").and_then(|t| t.as_str()) {
                                                // Append token to last message if it's from the agent
                                                let mut msgs = messages.read().clone();
                                                if let Some(last) = msgs.last_mut() {
                                                    if last.sender == "agent" {
                                                        last.content.push_str(token);
                                                        messages.set(msgs);
                                                        continue;
                                                    }
                                                }
                                                msgs.push(ChatMessage {
                                                    sender: "agent".to_string(),
                                                    content: token.to_string(),
                                                    is_tool: false,
                                                    tool_status: None,
                                                });
                                                messages.set(msgs);
                                            }
                                        }
                                        "status" => {
                                            if let Some(status) = val.get("status").and_then(|t| t.as_str()) {
                                                let mut msgs = messages.read().clone();
                                                msgs.push(ChatMessage {
                                                    sender: "system".to_string(),
                                                    content: status.to_string(),
                                                    is_tool: true,
                                                    tool_status: Some("running".to_string()),
                                                });
                                                messages.set(msgs);
                                            }
                                        }
                                        "complete" => {
                                            // Handle completion if needed
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    connection_status.set("Disconnected".to_string());
                });

                // Fetch initial library nodes
                let fetch_lib = json!({
                    "jsonrpc": "2.0",
                    "method": "library.list_nodes",
                    "params": {},
                    "id": "list-lib-1"
                });
                // Send library fetch request if possible
            }
            Err(_) => {
                connection_status.set("Connection Failed".to_string());
            }
        }
    });

    let send_message = move |_| {
        let text = input_text.read().trim().to_string();
        if text.is_empty() { return; }

        let mut msgs = messages.read().clone();
        msgs.push(ChatMessage {
            sender: "user".to_string(),
            content: text.clone(),
            is_tool: false,
            tool_status: None,
        });
        messages.set(msgs);
        input_text.set(String::new());

        // Send over WS
        if let Some(ref mut tx) = *ws_tx.write() {
            let req = json!({
                "jsonrpc": "2.0",
                "method": "intent.submit",
                "params": {
                    "page_id": "web-session",
                    "channel_id": "web",
                    "user_id": "web-user",
                    "content": text,
                    "attachments": [],
                    "metadata": {},
                    "timestamp": 0
                },
                "id": "intent-web-1"
            });
            let _ = futures_util::SinkExt::start_send(tx, Message::Text(req.to_string()));
        }
    };

    rsx! {
        style {
            r#"
            .app-container {{
                display: flex;
                height: 100vh;
                width: 100vw;
            }}
            .sidebar {{
                width: 260px;
                background-color: var(--sidebar-bg);
                border-right: 1px solid var(--border-color);
                display: flex;
                flex-direction: column;
                padding: 20px;
                box-sizing: border-box;
            }}
            .sidebar-header {{
                font-size: 24px;
                font-weight: 800;
                color: var(--primary);
                margin-bottom: 30px;
                display: flex;
                align-items: center;
                gap: 10px;
            }}
            .nav-button {{
                background: none;
                border: 1px solid transparent;
                color: var(--text-muted);
                padding: 12px 16px;
                text-align: left;
                font-size: 16px;
                font-weight: 600;
                cursor: pointer;
                border-radius: 8px;
                margin-bottom: 10px;
                transition: all 0.2s ease;
                display: flex;
                align-items: center;
                gap: 12px;
            }}
            .nav-button:hover, .nav-button.active {{
                background-color: var(--panel-bg);
                color: var(--text-main);
                border-color: var(--border-color);
                box-shadow: 0 0 15px var(--primary-glow);
            }}
            .main-content {{
                flex: 1;
                display: flex;
                flex-direction: column;
                background: transparent;
                position: relative;
            }}
            .top-bar {{
                height: 70px;
                border-bottom: 1px solid var(--border-color);
                display: flex;
                align-items: center;
                justify-content: space-between;
                padding: 0 30px;
                background-color: rgba(18, 14, 36, 0.3);
                backdrop-filter: blur(10px);
            }}
            .status-badge {{
                display: flex;
                align-items: center;
                gap: 8px;
                font-size: 14px;
                font-weight: 600;
            }}
            .status-dot {{
                width: 10px;
                height: 10px;
                border-radius: 50%;
            }}
            .status-dot.connected {{ background-color: var(--success); box-shadow: 0 0 10px var(--success); }}
            .status-dot.disconnected {{ background-color: #ef4444; }}
            .status-dot.connecting {{ background-color: #f59e0b; }}
            
            /* Chat styles */
            .chat-area {{
                flex: 1;
                display: flex;
                flex-direction: column;
                overflow: hidden;
                padding: 30px;
                box-sizing: border-box;
            }}
            .messages-list {{
                flex: 1;
                overflow-y: auto;
                margin-bottom: 20px;
                display: flex;
                flex-direction: column;
                gap: 15px;
                padding-right: 10px;
            }}
            .message-bubble {{
                max-width: 70%;
                padding: 14px 18px;
                border-radius: 16px;
                line-height: 1.5;
                font-size: 15px;
            }}
            .message-bubble.user {{
                align-self: flex-end;
                background: linear-gradient(135deg, var(--primary), var(--secondary));
                color: #ffffff;
                border-bottom-right-radius: 4px;
                box-shadow: 0 4px 15px rgba(168, 85, 247, 0.3);
            }}
            .message-bubble.agent {{
                align-self: flex-start;
                background-color: var(--panel-bg);
                border: 1px solid var(--border-color);
                border-bottom-left-radius: 4px;
            }}
            .message-bubble.system {{
                align-self: center;
                background-color: rgba(59, 130, 246, 0.1);
                border: 1px solid rgba(59, 130, 246, 0.2);
                color: var(--secondary);
                font-size: 13px;
                padding: 8px 14px;
                border-radius: 20px;
            }}
            .input-box {{
                display: flex;
                gap: 15px;
                background-color: var(--panel-bg);
                border: 1px solid var(--border-color);
                padding: 8px 15px;
                border-radius: 12px;
                align-items: center;
            }}
            .chat-input {{
                flex: 1;
                background: none;
                border: none;
                color: var(--text-main);
                font-size: 16px;
                outline: none;
                padding: 10px 0;
            }}
            .send-btn {{
                background: linear-gradient(135deg, var(--primary), var(--secondary));
                border: none;
                color: white;
                padding: 10px 20px;
                border-radius: 8px;
                font-weight: 600;
                cursor: pointer;
                transition: transform 0.1s ease;
            }}
            .send-btn:active {{
                transform: scale(0.95);
            }}

            /* Library styles */
            .library-area {{
                flex: 1;
                padding: 30px;
                overflow-y: auto;
            }}
            .library-grid {{
                display: grid;
                grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
                gap: 20px;
            }}
            .library-card {{
                background-color: var(--panel-bg);
                border: 1px solid var(--border-color);
                border-radius: 12px;
                padding: 20px;
                transition: all 0.2s ease;
                cursor: pointer;
            }}
            .library-card:hover {{
                border-color: var(--primary);
                transform: translateY(-2px);
                box-shadow: 0 5px 15px rgba(168, 85, 247, 0.1);
            }}
            .card-icon {{
                font-size: 24px;
                margin-bottom: 12px;
            }}
            .card-title {{
                font-size: 18px;
                font-weight: 600;
                margin-bottom: 8px;
            }}
            .card-meta {{
                font-size: 13px;
                color: var(--text-muted);
            }}
            "#
        }
        div { class: "app-container",
            div { class: "sidebar",
                div { class: "sidebar-header",
                    span { "👾" }
                    span { "Hydragent" }
                }
                button {
                    class: if *current_tab.read() == Tab::Chat { "nav-button active" } else { "nav-button" },
                    onclick: move |_| current_tab.set(Tab::Chat),
                    span { "💬" }
                    span { "Chat" }
                }
                button {
                    class: if *current_tab.read() == Tab::Library { "nav-button active" } else { "nav-button" },
                    onclick: move |_| current_tab.set(Tab::Library),
                    span { "📚" }
                    span { "Library" }
                }
            }
            div { class: "main-content",
                div { class: "top-bar",
                    h2 {
                        match *current_tab.read() {
                            Tab::Chat => "Agent Chat",
                            Tab::Library => "Knowledge Library"
                        }
                    }
                    div { class: "status-badge",
                        div {
                            class: match connection_status.read().as_str() {
                                "Connected" => "status-dot connected",
                                "Connecting..." => "status-dot connecting",
                                _ => "status-dot disconnected"
                            }
                        }
                        span { "{connection_status}" }
                    }
                }
                match *current_tab.read() {
                    Tab::Chat => rsx! {
                        div { class: "chat-area",
                            div { class: "messages-list",
                                for msg in messages.read().iter() {
                                    div {
                                        class: match msg.sender.as_str() {
                                            "user" => "message-bubble user",
                                            "agent" => "message-bubble agent",
                                            _ => "message-bubble system"
                                        },
                                        "{msg.content}"
                                    }
                                }
                            }
                            div { class: "input-box",
                                input {
                                    class: "chat-input",
                                    placeholder: "Type a message...",
                                    value: "{input_text}",
                                    oninput: move |evt| input_text.set(evt.value().clone()),
                                    onkeydown: move |evt| {
                                        if evt.key() == Key::Enter && !evt.modifiers().shift() {
                                            send_message(evt);
                                        }
                                    }
                                }
                                button { class: "send-btn", onclick: send_message, "Send" }
                            }
                        }
                    },
                    Tab::Library => rsx! {
                        div { class: "library-area",
                            div { class: "library-grid",
                                div { class: "library-card",
                                    div { class: "card-icon", "📁" }
                                    div { class: "card-title", "Default Shelf" }
                                    div { class: "card-meta", "Contains 3 books" }
                                }
                                div { class: "library-card",
                                    div { class: "card-icon", "📖" }
                                    div { class: "card-title", "Hydragent Manual" }
                                    div { class: "card-meta", "12 pages" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
