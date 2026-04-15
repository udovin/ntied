# ntied-transport

Encrypted UDP transport with post-quantum cryptography, reliable streams,
semi-reliable message channels, and key rotation.

## Architecture

Two layers:

- **`connection_v2`** — synchronous state machine. No I/O, no async, no allocator.
  Caller drives it via `send()`/`recv()`/`on_timeout()`. Owns all protocol logic:
  handshake, encryption, streams, channels, ACK, loss detection, rekeying.

- **`node_v2`** — async wrapper. Tokio event loop, UDP socket, accept/connect API.
  Calls into `connection_v2::Connection` behind `Arc<Mutex<>>`.

## Documentation

| Document | Description |
|----------|-------------|
| [PROTOCOL.md](PROTOCOL.md) | Wire format, packet/frame types, handshake, encryption, ACK |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Module structure, state machine, streams, channels, key rotation |

## External Dependencies

| Crate | Purpose |
|-------|---------|
| `ed25519-dalek` | Ed25519 signatures |
| `ml-dsa` | ML-DSA-65 post-quantum signatures |
| `x25519-dalek` | X25519 key exchange |
| `ml-kem` | ML-KEM-768 post-quantum KEM |
| `sha3` | SHA3-256 for PeerId |
| `rand` | Cryptographic RNG |
| `tokio` | Async runtime, UDP, timers |
