use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

/// Must mirror server's ClientMessage
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", content = "payload")]
pub enum ClientMessage {
    Join { username: String },
    Send { text: String },
    Leave,
}

/// Must mirror server's ServerMessage
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", content = "payload")]
pub enum ServerMessage {
    Chat { from: String, text: String },
    Info { text: String },
    Error { text: String },
}

#[tokio::main]
async fn main() {
    // --- Configuration via env vars or CLI args ---
    // Priority: CLI args > env vars > defaults
    // Usage: client [host] [port] [username]
    let args: Vec<String> = std::env::args().collect();

    let host = args
        .get(1)
        .cloned()
        .or_else(|| std::env::var("HOST").ok())
        .unwrap_or_else(|| "127.0.0.1".to_string());

    let port = args
        .get(2)
        .cloned()
        .or_else(|| std::env::var("PORT").ok())
        .unwrap_or_else(|| "8080".to_string());

    let username = args
        .get(3)
        .cloned()
        .or_else(|| std::env::var("USERNAME").ok())
        .expect("Username required: pass as 3rd argument or set USERNAME env var");

    let addr = format!("{host}:{port}");
    println!("Connecting to {addr} as '{username}'...");

    let stream = TcpStream::connect(&addr)
        .await
        .expect("Failed to connect to server");

    println!("Connected! Commands: `send <MSG>` | `leave`");

    let (reader, writer) = stream.into_split();
    let mut server_lines = BufReader::new(reader).lines();

    // Channel to send outgoing messages from stdin task -> writer task
    let (out_tx, mut out_rx) = mpsc::channel::<ClientMessage>(32);

    // --- Task 1: Write outgoing messages to server ---
    let write_task = tokio::spawn(async move {
        let mut writer = writer;
        // Send join
        let join = ClientMessage::Join {
            username: username.clone(),
        };
        write_msg(&mut writer, &join).await;

        while let Some(msg) = out_rx.recv().await {
            write_msg(&mut writer, &msg).await;
            if matches!(msg, ClientMessage::Leave) {
                break;
            }
        }
    });

    // --- Task 2: Read messages from server and print them ---
    let print_task = tokio::spawn(async move {
        while let Ok(Some(line)) = server_lines.next_line().await {
            match serde_json::from_str::<ServerMessage>(&line) {
                Ok(ServerMessage::Chat { from, text }) => {
                    println!("\r[{from}]: {text}");
                }
                Ok(ServerMessage::Info { text }) => {
                    println!("\r[INFO]: {text}");
                }
                Ok(ServerMessage::Error { text }) => {
                    eprintln!("\r[ERROR]: {text}");
                }
                Err(e) => eprintln!("Parse error: {e}"),
            }
            // Re-print the prompt after incoming message
            print!("> ");
            let _ = io::stdout().flush();
        }
    });

    // --- Task 3 (main): Read stdin and dispatch commands ---
    let stdin = tokio::io::stdin();
    let mut stdin_lines = BufReader::new(stdin).lines();

    loop {
        print!("> ");
        let _ = io::stdout().flush();

        match stdin_lines.next_line().await {
            Ok(Some(line)) => {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                if line == "leave" {
                    let _ = out_tx.send(ClientMessage::Leave).await;
                    break;
                } else if let Some(msg) = line.strip_prefix("send ") {
                    let _ = out_tx
                        .send(ClientMessage::Send {
                            text: msg.to_string(),
                        })
                        .await;
                } else {
                    eprintln!("Unknown command. Use: `send <MSG>` or `leave`");
                }
            }
            _ => break, // EOF / error
        }
    }

    // Graceful shutdown
    write_task.await.ok();
    print_task.abort();
    println!("Disconnected. Goodbye!");
}

async fn write_msg(writer: &mut tokio::net::tcp::OwnedWriteHalf, msg: &ClientMessage) {
    let mut s = serde_json::to_string(msg).unwrap();
    s.push('\n');
    if let Err(e) = writer.write_all(s.as_bytes()).await {
        eprintln!("Write error: {e}");
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_join() {
        let msg = ClientMessage::Join {
            username: "alice".into(),
        };
        let s = serde_json::to_string(&msg).unwrap();
        assert!(s.contains("Join"));
        assert!(s.contains("alice"));
    }

    #[test]
    fn serialize_send() {
        let msg = ClientMessage::Send {
            text: "hello world".into(),
        };
        let s = serde_json::to_string(&msg).unwrap();
        assert!(s.contains("Send"));
        assert!(s.contains("hello world"));
    }

    #[test]
    fn serialize_leave() {
        let msg = ClientMessage::Leave;
        let s = serde_json::to_string(&msg).unwrap();
        assert!(s.contains("Leave"));
    }

    #[test]
    fn deserialize_chat() {
        let s = r#"{"type":"Chat","payload":{"from":"bob","text":"hi"}}"#;
        let msg: ServerMessage = serde_json::from_str(s).unwrap();
        assert!(
            matches!(msg, ServerMessage::Chat { ref from, ref text } if from == "bob" && text == "hi")
        );
    }

    #[test]
    fn deserialize_info() {
        let s = r#"{"type":"Info","payload":{"text":"alice joined"}}"#;
        let msg: ServerMessage = serde_json::from_str(s).unwrap();
        assert!(matches!(msg, ServerMessage::Info { ref text } if text == "alice joined"));
    }

    #[test]
    fn deserialize_error() {
        let s = r#"{"type":"Error","payload":{"text":"Username already taken"}}"#;
        let msg: ServerMessage = serde_json::from_str(s).unwrap();
        assert!(matches!(msg, ServerMessage::Error { ref text } if text.contains("taken")));
    }

    #[test]
    fn test_command_parse_send() {
        let line = "send hello world";
        assert!(line.starts_with("send "));
        let text = line.strip_prefix("send ").unwrap();
        assert_eq!(text, "hello world");
    }

    #[test]
    fn test_command_parse_leave() {
        let line = "leave";
        assert_eq!(line, "leave");
    }
}
