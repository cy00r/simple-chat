# Simple Chat

A minimal async TCP chat server and CLI client written in Rust with Tokio.

## Demo

> https://github.com/user-attachments/assets/269ce9b6-45f4-4f91-a5ab-45433cd9feeb

## Architecture

```
┌──────────┐   JSON/TCP   ┌────────────────────────────────────────┐
│  client  │ ──────────► │  server                                 │
│          │ ◄──────────  │  • broadcast channel (256 capacity)    │
└──────────┘              │  • one task per connection (non-block) │
                          │  • HashMap<username, addr> for users   │
                          └────────────────────────────────────────┘
```

### Protocol

All messages are newline-delimited JSON.

**Client → Server**

| Message | Payload | Description |
|---------|---------|-------------|
| `Join`  | `{ username }` | Must be first message; rejected if name taken |
| `Send`  | `{ text }` | Broadcast text to all other users |
| `Leave` | — | Graceful disconnect |

**Server → Client**

| Message | Payload | Description |
|---------|---------|-------------|
| `Chat`  | `{ from, text }` | A message from another user |
| `Info`  | `{ text }` | Join/leave announcements |
| `Error` | `{ text }` | e.g. duplicate username |

## Running

### Prerequisites

```bash
rustup toolchain install stable
```

### Install the pre-commit hook (optional but recommended)

```bash
git config core.hooksPath .githooks
chmod +x .githooks/pre-commit
```

### Start the server

```bash
# defaults: HOST=127.0.0.1 PORT=8080
cargo run -p server

# or with custom host/port
HOST=0.0.0.0 PORT=9000 cargo run -p server
```

### Start a client

```bash
# Pass host, port, username as positional args …
cargo run -p client -- 127.0.0.1 8080 alice

# … or via env vars
HOST=127.0.0.1 PORT=8080 USERNAME=alice cargo run -p client
```

### Client commands

```
> send Hello everyone!    # broadcast a message
> leave                   # disconnect and exit
```

### Multi-user example (three terminals)

```
# Terminal 1
cargo run -p server

# Terminal 2
cargo run -p client -- 127.0.0.1 8080 alice

# Terminal 3
cargo run -p client -- 127.0.0.1 8080 bob
```

## Testing

```bash
cargo test --all
```

## CI / CD

The GitHub Actions workflow (`.github/workflows/ci.yml`) runs on every push and PR:

1. **Format check** – `cargo fmt --all -- --check`
2. **Clippy** – zero warnings allowed
3. **Unit + integration tests** – `cargo test --all`
4. **E2E smoke test** – starts the server, connects with the client, sends a message, then leaves

## Design decisions

- **`broadcast::channel`** – single channel for all users; each connection task subscribes. Old messages are dropped for lagging readers rather than blocking the sender.
- **`OwnedWriteHalf` behind `Arc<Mutex<…>>`** – lets the forward task and the main connection task share the TCP writer safely without an extra channel.
- **Non-blocking everywhere** – every I/O call is async; no `std::thread::sleep` or blocking reads.
- **Small memory footprint** – one `broadcast::Sender`, one `HashMap` entry per user, one task pair (read + forward) per connection.
