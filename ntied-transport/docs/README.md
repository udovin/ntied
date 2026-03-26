# ntied-transport

UDP-based peer-to-peer transport protocol with post-quantum hybrid cryptography,
reliable/unreliable channels, NAT hole punching, and relay support.

## What works today

End-to-end encrypted peer-to-peer communication over UDP:

- **Crypto** — hybrid Ed25519 + ML-DSA-65 identity, X25519 + ML-KEM-768 key exchange, ChaCha20-Poly1305 AEAD.
- **Handshake** — 1-RTT key exchange → encrypted Auth with transcript hash → Established.
- **Key rotation** — tri-state epoch rotation (`Next`, `Current`, `Previous`), duplicate/simultaneous rekey handling.
- **Packet layer** — ACK tracking, gap + timeout loss detection, RTT measurement, retransmission.
- **Streams** — reliable ordered byte streams with offset tracking, reorder buffer, flow control, FIN. Reliable datagrams with fragmentation, reassembly, deduplication.
- **Stream manager** — open, close, accept, multiplex multiple streams and datagram channels per connection.
- **Net layer** — `PeerConnection` coordinator: decrypt → dispatch frames → collect → encrypt → send.
- **Discovery** — `Discovery` trait (`resolve` / `register` / `recv_connection_request`), `HashMapDiscovery` for testing, `ServerDiscovery` ported from v1.
- **NAT hole punching** — `HolePunch` packet sending on both sides (initiator + responder), multi-packet burst with auto-cancellation on response.
- **Public API** — `Transport::bind`, `connect` (by `PeerId`), `accept`, `Connection`, `ReliableStream`, `DatagramStream` over real UDP sockets.

Verified with integration tests: two peers discover each other, complete a handshake,
open streams, and exchange data (multi-message, bidirectional, large payloads) — all through encrypted UDP.

## Documentation

| Document | Description |
|----------|-------------|
| [PROTOCOL.md](PROTOCOL.md) | Full protocol specification: wire format, handshake, frames, ACK, channels, key rotation, NAT, relay |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Module structure, implemented types and APIs, layer dependencies, public API |
| [PLAN.md](PLAN.md) | Gateway, relay, DHT — architecture decisions, new frame types, phased implementation plan |

---

## Implementation Status

### crypto/ — Cryptographic Primitives

| Module | Status | Description |
|--------|--------|-------------|
| `identity.rs` | ✅ Done | `PrivateKey`, `PublicKey`, `Signature`, `PeerId` — hybrid Ed25519 + ML-DSA-65 |
| `kem.rs` | ✅ Done | `EphemeralPrivateKey`, `EphemeralPublicKey`, `KemCiphertext`, `SharedSecret` — hybrid X25519 + ML-KEM-768 |
| `aead.rs` | ✅ Done | `EncryptionKeys`, `EncryptionKey` — HKDF-SHA3-256 key derivation + ChaCha20-Poly1305 AEAD |

### wire/ — Wire Format

| Module | Status | Description |
|--------|--------|-------------|
| `codec.rs` | ✅ Done | `Reader`, `Writer` — binary reader/writer utilities (big-endian) |
| `packet.rs` | ✅ Done | `Packet`, `KeyExchangeInit`, `KeyExchangeResponse`, `Data`, `HolePunch`, `Relay` — outer packet types |
| `frame.rs` | ✅ Done | `Frame`, 15 frame types — Ack, Ping/Pong, StreamOpen, StreamData, Auth, Rekey, etc. |

### session/ — Session State Machines

| Module | Status | Description |
|--------|--------|-------------|
| `facade.rs` | ✅ Done | `Session`: state management and event-driven frame processing |
| `state.rs` | ✅ Done | `CryptoState`: encrypt/decrypt, tri-state epoch rotation |
| `handshake.rs` | ✅ Done | `AuthState`: Phase 2 auth assembly, signature verification with transcript hash |
| `rekey.rs` | ✅ Done | `RekeyState`: KEM key exchange, handling duplicates, simultaneous rotation |
| `fragment.rs` | ✅ Done | `FragmentCollector`: generic assembler for crypto frames |

### packet/ — Packet-Level Mechanisms

| Module | Status | Description |
|--------|--------|-------------|
| `loss.rs` | ✅ Done | `RecvAckState`, `SendAckState` — ACK tracking, loss detection (gap + timeout), RTT measurement |
| `congestion.rs` | ⬜ Todo | Congestion control and send pacing |

### stream/ — Stream Management

| Module | Status | Description |
|--------|--------|-------------|
| `reliable.rs` | ✅ Done | `ReliableSendStream`, `ReliableRecvStream` — offset tracking, reorder buffer, flow control, FIN |
| `manager.rs` | ✅ Done | `StreamManager` — open, close, accept, read, write, multiplex, flow control |
| `datagram.rs` | ✅ Done | `DatagramSender`, `DatagramReceiver` — reliable datagram: fragmentation, reassembly, deduplication |

### net/ — Connection Coordinator

| Module | Status | Description |
|--------|--------|-------------|
| `connection.rs` | ✅ Done | `PeerConnection` — decrypt, dispatch frames, session events, stream I/O, ACK, packet building |

### discovery/ — Peer Discovery

| Module | Status | Description |
|--------|--------|-------------|
| `traits.rs` | ✅ Done | `Discovery` trait — `resolve`, `register`, `recv_connection_request`; `ConnectionRequest` |
| `hashmap.rs` | ✅ Done | `HashMapDiscovery` — in-memory `HashMap<PeerId, SocketAddr>` for testing |
| `server.rs` | ✅ Done | `ServerDiscovery` — centralized discovery server, incoming connection notifications |
| `dht.rs` | ⬜ Todo | DHT-based discovery |

### api.rs — Public API

| Module | Status | Description |
|--------|--------|-------------|
| `api.rs` | ✅ Done | `Transport`, `Connection`, `ReliableStream`, `DatagramStream` — bind, connect by PeerId, accept, open/accept streams, send/recv, NAT hole punching, keepalive, graceful close |

---

## Remaining Work

### Must have

| Gap | Description | Effort |
|-----|-------------|--------|
| **Congestion control** | No send pacing. Without it, a fast sender can saturate the network and cause packet loss spirals. | Medium |
| **Rekey timer** | Rekey state machine is complete, but nothing triggers periodic rekeying. Long-lived connections reuse the same keys indefinitely. | Small |
| **DHT discovery** | `DhtDiscovery` (mainline DHT + STUN) not yet ported to the `Discovery` trait. | Small |

### Nice to have

| Gap | Description | Effort |
|-----|-------------|--------|
| **Relay support** | `Relay` packet type is defined but not handled. Needed for symmetric NAT fallback. | Medium |
| **Connection error propagation** | `recv_loop` silently swallows socket errors. API methods return "connection gone" but don't distinguish cause (timeout, reset, close). | Small |

---

## External Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `ed25519-dalek` | 2 | Ed25519 signatures |
| `ml-dsa` | 0.0.4 | ML-DSA-65 post-quantum signatures |
| `x25519-dalek` | 2 | X25519 key exchange |
| `ml-kem` | 0.2 | ML-KEM-768 post-quantum KEM |
| `kem` | =0.3.0-pre.0 | `Encapsulate`/`Decapsulate` traits for ml-kem |
| `sha3` | 0.10 | SHA3-256 for PeerId derivation |
| `hybrid-array` | 0.3 | Array type interop with ml-dsa |
| `rand` | 0.8 | Cryptographic RNG |
| `base64` | 0.22 | PeerId string encoding |
| `async-trait` | workspace | Async trait support for Discovery |
| `tokio` | workspace | Async runtime, UDP socket, timers, sync primitives |