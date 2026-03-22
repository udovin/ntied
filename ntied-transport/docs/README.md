# ntied-transport v2

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
| `server.rs` | ✅ Done | `ServerDiscovery` — centralized discovery server (ported from v1), incoming connection notifications |
| `dht.rs` | ⬜ Todo | DHT-based discovery (v1 has `DhtDiscovery`) |

### api.rs — Public API

| Module | Status | Description |
|--------|--------|-------------|
| `api.rs` | ✅ Done | `Transport`, `Connection`, `ReliableStream` — bind, connect by PeerId, accept, open/accept streams, send/recv, NAT hole punching, keepalive, graceful close |

---

## Suggested Implementation Order

Modules are listed in dependency order — each depends only on those above it.

1. ~~**crypto/aead.rs**~~ ✅
2. ~~**wire/codec.rs**~~ ✅
3. ~~**wire/packet.rs**~~ ✅
4. ~~**wire/frame.rs**~~ ✅
5. ~~**session/**~~ ✅ (facade, state, handshake, rekey, fragment)
6. ~~**packet/loss.rs**~~ ✅ — ACK tracking, loss detection, RTT
7. ~~**stream/reliable.rs**~~ ✅ — reliable ordered stream
8. ~~**stream/manager.rs**~~ ✅ — stream lifecycle, multiplex
9. ~~**net/connection.rs**~~ ✅ — PeerConnection coordinator
10. ~~**discovery/traits.rs + hashmap.rs**~~ ✅ — Discovery trait, test implementation
11. ~~**api.rs**~~ ✅ (basic) — Transport, Connection, ReliableStream
12. **packet/congestion.rs** — congestion control
13. ~~**stream/datagram.rs**~~ ✅ — reliable datagram fragmentation
15. ~~**discovery/server.rs**~~ ✅ — centralized discovery (ported from v1)
16. **discovery/dht.rs** — DHT discovery (port from v1)
17. ~~**NAT hole punching**~~ ✅ — HolePunch packet handling, multi-packet burst, auto-cancel
18. **Relay support** — Relay packet wrapping/unwrapping
19. **Graceful close** — ConnectionClose frame handling
20. **Keepalive / rekey timers** — Ping scheduling, rekey trigger

---

## What's Missing for v2 Migration

The v2 core works end-to-end (handshake, streams, data exchange over real UDP),
but several pieces are needed before it can replace v1 in production.

### Must have

| Gap | Description | Effort |
|-----|-------------|--------|
| **Discovery implementations** | `ServerDiscovery` ported. `DhtDiscovery` still missing. | Small |
| **Congestion control** | v2 has no send pacing. Without it, a fast sender can saturate the network and cause packet loss spirals. | Medium |
| ~~**Keepalive timer**~~ | ✅ Done — Ping scheduled every 5 s, connection dropped after 30 s without response. | ~~Small~~ |
| **Rekey timer** | Rekey state machine is complete, but nothing triggers periodic rekeying. Long-lived connections reuse the same keys indefinitely. | Small |
| ~~**Graceful connection close**~~ | ✅ Done — `Connection::close()` sends `ConnectionClose` frame; `Drop` impl triggers close automatically. | ~~Small~~ |
| ~~**`Connection::peer_id` / `peer_identity`**~~ | ✅ Done — `Connection::peer_public_key()` and `Connection::peer_id()` exposed. | ~~Small~~ |
| **Crate re-exports** | v2 types live under `v2::api::*`, `v2::discovery::*`, etc. Need top-level re-exports or a feature flag to switch the default public API. | Small |

### Nice to have

| Gap | Description | Effort |
|-----|-------------|--------|
| ~~**NAT hole punching**~~ | ✅ Done — Initiator sends HolePunch before KeyExchangeInit; responder sends HolePunch on `recv_connection_request`. Multi-packet burst (4×150 ms), auto-cancelled on any response from peer addr. | ~~Medium~~ |
| **Relay support** | `Relay` packet type is defined but not handled. Needed for symmetric NAT fallback. | Medium |
| **Connection error propagation** | `recv_loop` silently swallows socket errors. API methods return "connection gone" but don't distinguish cause (timeout, reset, close). | Small |

### API differences from v1

| v1 | v2 | Note |
|----|-----|------|
| `connect(&PublicKey)` | `connect(&PeerId)` | Identity model changed; PeerId is a 33-byte hash of the hybrid public key |
| `Connection::send(impl Into<Vec<u8>>)` / `recv()` | `ReliableStream::send(&[u8])` / `recv()` | v1 has one implicit channel per connection; v2 multiplexes named streams |
| `bind(addr, key, server_addr)` | `bind(addr, key, Arc<dyn Discovery>)` | Discovery is now a trait, not hardcoded to a server |
| `peer_public_key()` on Connection | `peer_public_key()` + `peer_id()` | Implemented |
| Heartbeat + key rotation automatic | Keepalive wired; rekey timer still missing | Ping every 5 s, timeout 30 s |

---

## External Dependencies (v2-specific)

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