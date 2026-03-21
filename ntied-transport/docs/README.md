# ntied-transport v2

UDP-based peer-to-peer transport protocol with post-quantum hybrid cryptography,
reliable/unreliable channels, NAT hole punching, and relay support.

## Documentation

| Document | Description |
|----------|-------------|
| [PROTOCOL.md](PROTOCOL.md) | Full protocol specification: wire format, handshake, frames, ACK, channels, key rotation, NAT, relay |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Module structure, implemented types and APIs, layer dependencies, target public API |

---

## Implementation Status

### crypto/ — Cryptographic Primitives

| Module | Status | Description |
|--------|--------|-------------|
| `identity.rs` | ✅ Done | `PrivateKey`, `PublicKey`, `Signature`, `PeerId` — hybrid Ed25519 + ML-DSA-65 |
| `kem.rs` | ✅ Done | `EphemeralPrivateKey`, `EphemeralPublicKey`, `KemCiphertext`, `SharedSecret` — hybrid X25519 + ML-KEM-768 |
| `aead.rs` | ⬜ Todo | ChaCha20-Poly1305 AEAD encrypt/decrypt |
| `kdf.rs` | ⬜ Todo | HKDF-SHA256 extract/expand |

### wire/ — Wire Format

| Module | Status | Description |
|--------|--------|-------------|
| `codec.rs` | ⬜ Todo | Binary reader/writer utilities |
| `packet.rs` | ⬜ Todo | Outer packet types: KeyExchangeInit, KeyExchangeResponse, Data, HolePunch, Relay |
| `frame.rs` | ⬜ Todo | Inner frame types: Ack, Ping/Pong, StreamData, Auth, Rekey, etc. |

### session/ — Session State Machines

| Module | Status | Description |
|--------|--------|-------------|
| `handshake.rs` | ⬜ Todo | Two-phase handshake: key exchange + authentication |
| `state.rs` | ⬜ Todo | Active session: encrypt/decrypt, counter tracking, epoch |
| `rekey.rs` | ⬜ Todo | Key rotation state machine |

### stream/ — Stream Management

| Module | Status | Description |
|--------|--------|-------------|
| `reliable.rs` | ⬜ Todo | Reliable ordered stream: offset tracking, reorder buffer |
| `datagram.rs` | ⬜ Todo | Reliable datagram: fragmentation, reassembly |
| `unreliable.rs` | ⬜ Todo | Unreliable datagram: passthrough |
| `manager.rs` | ⬜ Todo | Stream lifecycle: open, close, multiplex |

### packet/ — Packet-Level Mechanisms

| Module | Status | Description |
|--------|--------|-------------|
| `assembler.rs` | ⬜ Todo | Pack frames into MTU-sized packets |
| `loss.rs` | ⬜ Todo | ACK processing, loss detection, retransmission |
| `congestion.rs` | ⬜ Todo | Congestion control and send pacing |

### net/ — I/O Layer

| Module | Status | Description |
|--------|--------|-------------|
| `socket.rs` | ⬜ Todo | UDP socket wrapper |
| `router.rs` | ⬜ Todo | Main event loop: recv → dispatch → send |
| `direct.rs` | ⬜ Todo | Direct peer-to-peer link |
| `relay.rs` | ⬜ Todo | Relay link (wraps packets in Relay envelope) |

### discovery/ — Peer Discovery

| Module | Status | Description |
|--------|--------|-------------|
| `traits.rs` | ⬜ Todo | Discovery trait definition |
| `server.rs` | ⬜ Todo | Centralized discovery server |
| `dht.rs` | ⬜ Todo | DHT-based discovery |

### api.rs — Public API

| Module | Status | Description |
|--------|--------|-------------|
| `api.rs` | ⬜ Todo | `Transport`, `Connection`, `ReliableStream`, `DatagramChannel` |

---

## Suggested Implementation Order

Modules are listed in dependency order — each depends only on those above it.

1. **crypto/aead.rs** — ChaCha20-Poly1305 encrypt/decrypt
2. **crypto/kdf.rs** — HKDF-SHA256 extract/expand
3. **wire/codec.rs** — binary reader/writer
4. **wire/packet.rs** — outer packet serialization
5. **wire/frame.rs** — inner frame serialization
6. **session/handshake.rs** — two-phase handshake state machine
7. **session/state.rs** — active session encrypt/decrypt
8. **packet/assembler.rs** — frame → packet packing
9. **packet/loss.rs** — ACK + loss detection
10. **stream/reliable.rs** — reliable ordered stream
11. **stream/datagram.rs** — reliable datagram fragmentation
12. **stream/unreliable.rs** — unreliable datagram
13. **stream/manager.rs** — stream lifecycle
14. **session/rekey.rs** — key rotation
15. **packet/congestion.rs** — congestion control
16. **net/** — I/O layer
17. **discovery/** — peer discovery
18. **api.rs** — public API

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