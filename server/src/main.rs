use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex};

/// Messages sent from client to server
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", content = "payload")]
pub enum ClientMessage {
    Join { username: String },
    Send { text: String },
    Leave,
}

/// Messages sent from server -> client
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", content = "payload")]
pub enum ServerMessage {
    /// A chat message broadcast to other users
    Chat { from: String, text: String },
    /// Acknowledgement / status messages
    Info { text: String },
    /// Error (e.g. username taken)
    Error { text: String },
}

/// Shared state: maps username -> sender so we can detect duplicates
type Users = Arc<Mutex<HashMap<String, SocketAddr>>>;

#[tokio::main]
async fn main() {
    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let addr = format!("{host}:{port}");

    let listener = TcpListener::bind(&addr).await.expect("Failed to bind");
    println!("Server listening on {addr}");

    // broadcast channel – capacity 256, old messages dropped for slow readers
    let (tx, _rx) = broadcast::channel::<ServerMessage>(256);
    let users: Users = Arc::new(Mutex::new(HashMap::new()));

    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                let tx = tx.clone();
                let users = Arc::clone(&users);
                tokio::spawn(handle_connection(stream, addr, tx, users));
            }
            Err(e) => eprintln!("Accept error: {e}"),
        }
    }
}

async fn handle_connection(
    stream: TcpStream,
    addr: SocketAddr,
    tx: broadcast::Sender<ServerMessage>,
    users: Users,
) {
    let (reader, writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let writer = Arc::new(Mutex::new(writer));

    // Subscribe to broadcast BEFORE we read the join message so we don't miss anything
    let mut rx = tx.subscribe();

    // First message must be Join
    let username = loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            _ => return, // client disconnected before joining
        };
        match serde_json::from_str::<ClientMessage>(&line) {
            Ok(ClientMessage::Join { username }) => {
                let mut locked = users.lock().await;
                if locked.contains_key(&username) {
                    let msg = ServerMessage::Error {
                        text: "Username already taken".to_string(),
                    };
                    let _ = send_msg(&writer, &msg).await;
                    return;
                }
                locked.insert(username.clone(), addr);
                drop(locked);

                let info = ServerMessage::Info {
                    text: format!("{username} joined the chat"),
                };
                let _ = tx.send(info);
                break username;
            }
            _ => {
                let _ = send_msg(
                    &writer,
                    &ServerMessage::Error {
                        text: "Send Join message first".to_string(),
                    },
                )
                .await;
            }
        }
    };

    // Task: forward broadcast messages to this client
    let writer_clone = Arc::clone(&writer);
    let username_clone = username.clone();
    let forward_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    // Don't echo back messages sent by this user
                    if let ServerMessage::Chat { ref from, .. } = msg {
                        if from == &username_clone {
                            continue;
                        }
                    }
                    if send_msg(&writer_clone, &msg).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("Client {username_clone} lagged by {n} messages");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Main loop: read messages from this client
    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            _ => break, // disconnected
        };

        match serde_json::from_str::<ClientMessage>(&line) {
            Ok(ClientMessage::Send { text }) => {
                let msg = ServerMessage::Chat {
                    from: username.clone(),
                    text,
                };
                let _ = tx.send(msg);
            }
            Ok(ClientMessage::Leave) | Ok(ClientMessage::Join { .. }) => break,
            Err(e) => eprintln!("Parse error from {username}: {e}"),
        }
    }

    // Cleanup
    forward_task.abort();
    users.lock().await.remove(&username);
    let _ = tx.send(ServerMessage::Info {
        text: format!("{username} left the chat"),
    });
    println!("{username} disconnected");
}

/// Serialize and write a server message to the writer
async fn send_msg(
    writer: &Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    msg: &ServerMessage,
) -> std::io::Result<()> {
    let mut json = serde_json::to_string(msg).unwrap();
    json.push('\n');
    writer.lock().await.write_all(json.as_bytes()).await
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    /// Spin up a real server on a random port and return the address.
    async fn start_test_server() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, _) = broadcast::channel::<ServerMessage>(256);
        let users: Users = Arc::new(Mutex::new(HashMap::new()));
        tokio::spawn(async move {
            loop {
                let (stream, peer) = listener.accept().await.unwrap();
                let tx = tx.clone();
                let users = Arc::clone(&users);
                tokio::spawn(handle_connection(stream, peer, tx, users));
            }
        });
        addr
    }

    async fn connect(
        addr: SocketAddr,
    ) -> (
        tokio::net::tcp::OwnedWriteHalf,
        tokio::io::Lines<BufReader<tokio::net::tcp::OwnedReadHalf>>,
    ) {
        let stream = TcpStream::connect(addr).await.unwrap();
        let (r, w) = stream.into_split();
        (w, BufReader::new(r).lines())
    }

    async fn write_msg(w: &mut tokio::net::tcp::OwnedWriteHalf, msg: &ClientMessage) {
        let mut s = serde_json::to_string(msg).unwrap();
        s.push('\n');
        w.write_all(s.as_bytes()).await.unwrap();
    }

    async fn read_msg(
        lines: &mut tokio::io::Lines<BufReader<tokio::net::tcp::OwnedReadHalf>>,
    ) -> ServerMessage {
        let line = lines.next_line().await.unwrap().unwrap();
        serde_json::from_str(&line).unwrap()
    }

    // ── Unit tests ──────────────────────────────────────────────────────────

    #[test]
    fn client_message_serialization() {
        let msg = ClientMessage::Join {
            username: "alice".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, ClientMessage::Join { ref username } if username == "alice"));
    }

    #[test]
    fn server_message_serialization() {
        let msg = ServerMessage::Chat {
            from: "bob".to_string(),
            text: "hello".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(decoded, ServerMessage::Chat { ref from, ref text } if from == "bob" && text == "hello")
        );
    }

    // ── Integration tests ───────────────────────────────────────────────────

    #[tokio::test]
    async fn test_join_and_receive_info() {
        let addr = start_test_server().await;
        let (mut w, mut lines) = connect(addr).await;

        write_msg(
            &mut w,
            &ClientMessage::Join {
                username: "alice".to_string(),
            },
        )
        .await;

        // Server broadcasts "alice joined the chat" – alice herself also gets it via broadcast
        // (she's a subscriber). The Info message is not filtered, only Chat is filtered.
        let msg = read_msg(&mut lines).await;
        assert!(matches!(msg, ServerMessage::Info { ref text } if text.contains("alice")));
    }

    #[tokio::test]
    async fn test_duplicate_username_rejected() {
        let addr = start_test_server().await;

        let (mut w1, _lines1) = connect(addr).await;
        write_msg(
            &mut w1,
            &ClientMessage::Join {
                username: "bob".to_string(),
            },
        )
        .await;
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let (mut w2, mut lines2) = connect(addr).await;
        write_msg(
            &mut w2,
            &ClientMessage::Join {
                username: "bob".to_string(),
            },
        )
        .await;

        let msg = read_msg(&mut lines2).await;
        assert!(matches!(msg, ServerMessage::Error { ref text } if text.contains("taken")));
    }

    #[tokio::test]
    async fn test_message_broadcast_to_others() {
        let addr = start_test_server().await;

        // alice joins
        let (mut wa, mut la) = connect(addr).await;
        write_msg(
            &mut wa,
            &ClientMessage::Join {
                username: "alice".to_string(),
            },
        )
        .await;
        // drain alice's join info
        read_msg(&mut la).await;

        // bob joins – alice gets bob's join info
        let (mut wb, mut lb) = connect(addr).await;
        write_msg(
            &mut wb,
            &ClientMessage::Join {
                username: "bob".to_string(),
            },
        )
        .await;
        // drain bob's join info (alice sees it, bob sees it)
        read_msg(&mut la).await; // alice sees "bob joined"
        read_msg(&mut lb).await; // bob sees "bob joined"

        // bob sends a message
        write_msg(
            &mut wb,
            &ClientMessage::Send {
                text: "hi alice!".to_string(),
            },
        )
        .await;

        // alice should receive it
        let msg = read_msg(&mut la).await;
        assert!(
            matches!(msg, ServerMessage::Chat { ref from, ref text } if from == "bob" && text == "hi alice!")
        );

        // Give bob a moment – he should NOT receive his own message
        // We verify by sending another message from alice and checking bob gets that
        write_msg(
            &mut wa,
            &ClientMessage::Send {
                text: "hey bob".to_string(),
            },
        )
        .await;
        let msg = read_msg(&mut lb).await;
        assert!(matches!(msg, ServerMessage::Chat { ref from, .. } if from == "alice"));
    }

    #[tokio::test]
    async fn test_leave_removes_user() {
        let addr = start_test_server().await;

        let (mut wa, mut la) = connect(addr).await;
        write_msg(
            &mut wa,
            &ClientMessage::Join {
                username: "charlie".to_string(),
            },
        )
        .await;
        read_msg(&mut la).await; // join info

        let (mut wb, mut lb) = connect(addr).await;
        write_msg(
            &mut wb,
            &ClientMessage::Join {
                username: "dave".to_string(),
            },
        )
        .await;
        read_msg(&mut la).await; // charlie sees dave join
        read_msg(&mut lb).await; // dave sees own join

        // dave leaves
        write_msg(&mut wb, &ClientMessage::Leave).await;
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // charlie should see "dave left"
        let msg = read_msg(&mut la).await;
        assert!(matches!(msg, ServerMessage::Info { ref text } if text.contains("dave")));
    }
}
