# ntied-transport — v1 → v2 Migration Plan

This document describes how to migrate the `ntied` application crate from the
v1 transport API to v2.

---

## Current v1 usage in ntied

### Imported types

| Type | Used in | How |
|------|---------|-----|
| `Transport` | `contact/manager.rs` | `bind`, `connect`, `accept` |
| `Connection` | `contact/handle.rs` | `send`, `recv`, `peer_public_key` |
| `Error` | `contact/handle.rs` | Error handling |
| `ntied_crypto::PrivateKey` | `contact/manager.rs`, `config/mod.rs` | Identity generation and storage |
| `ntied_crypto::PublicKey` | everywhere | Peer addressing, contact map key, listener params |

### Call sites

```
contact/manager.rs:
  Transport::bind("0.0.0.0:0", private_key, server_addr)
  transport.accept()                         → Connection
  connection.peer_public_key()               → &PublicKey (sync)
  contacts: HashMap<PublicKey, ContactHandle> — PublicKey as map key

contact/handle.rs:
  transport.connect(&public_key)             → Connection
  connection.send(bytes)                     → send raw Vec<u8>
  connection.recv()                          → recv raw Vec<u8>
  connection.peer_public_key()               → &PublicKey (sync)

contact/listener.rs, call/listener.rs, chat/listener.rs:
  all callbacks take PublicKey as peer identifier
```

### Data flow

All application data is serialized as `bincode::serialize(&Packet)` → `Vec<u8>`,
sent over a single implicit channel per v1 `Connection`. Three packet families
share the same channel:

| Family | Used for |
|--------|----------|
| `ContactPacket` | Contact request / accept / reject |
| `ChatPacket` | Chat messages + acks + conflicts |
| `CallPacket` | Call signaling + audio/video data |

---

## API differences: v1 → v2

| Aspect | v1 | v2 |
|--------|----|----|
| **Bind** | `Transport::bind(addr, PrivateKey, SocketAddr)` | `Transport::bind(addr, PrivateKey, Arc<dyn Discovery>)` |
| **Identity** | `ntied_crypto::PrivateKey` (ECDSA-P256) | `v2::crypto::PrivateKey` (hybrid Ed25519 + ML-DSA-65) |
| **Peer identity** | `ntied_crypto::PublicKey` everywhere | `PeerId` (33-byte hash) for addressing; `PublicKey` (1984 B) available after auth |
| **Connect** | `transport.connect(&PublicKey)` | `transport.connect(&PeerId)` |
| **Accept result** | `Connection` with `peer_public_key()` sync | `Connection` with `peer_id().await` → `Option<PeerId>`, `peer_public_key().await` → `Option<PublicKey>` |
| **Data channel** | One implicit channel: `connection.send(bytes)` / `recv()` | Multiplexed streams: must `open_stream(purpose)` / `accept_stream()` first |
| **Stream types** | N/A | `ReliableStream` (byte stream) and `DatagramStream` (message-oriented) |
| **Error type** | `Error` = `String`-based | `io::Error` |
| **Discovery** | Hardcoded `ServerDiscoveryFactory` | `Arc<dyn Discovery>` — `ServerDiscovery`, `HashMapDiscovery`, etc. |
| **NAT traversal** | Manual hole punching in `Connection::connect` / `accept_with_holepunch` | Automatic — `recv_connection_request` + HolePunch burst in `recv_loop` |

---

## Migration strategy

Migrate in three phases. Each phase is independently testable and deployable.

### Phase 1 — Compatibility wrapper

Create a thin adapter module (`ntied/src/transport.rs`) that wraps v2 types
and exposes a v1-like API. This lets existing `contact/` code continue working
with minimal changes while the underlying transport is v2.

#### Wrapper: `NtiedTransport`

Wraps `v2::Transport`.

```rust
pub struct NtiedTransport {
    inner: v2::Transport,
}

impl NtiedTransport {
    pub async fn bind(addr: &str, private_key: v2::crypto::PrivateKey, server_addr: SocketAddr) -> io::Result<Self> {
        let socket = Arc::new(UdpSocket::bind(addr.parse()?).await?);
        let discovery = Arc::new(ServerDiscovery::new_shared(socket.clone(), server_addr));
        let inner = v2::Transport::bind_with_socket(socket, private_key, discovery).await?;
        Ok(Self { inner })
    }

    pub async fn connect(&self, peer_id: &PeerId) -> io::Result<NtiedConnection> { ... }
    pub async fn accept(&self) -> io::Result<NtiedConnection> { ... }
}
```

#### Wrapper: `NtiedConnection`

Wraps `v2::Connection` + a single `DatagramStream` opened with a well-known
purpose (e.g. `purpose = 0x0001`). This emulates v1's single-channel model.
`DatagramStream` is message-oriented (each `send` = one message, each `recv` =
one message), which matches v1's `send(Vec<u8>)` / `recv() -> Vec<u8>` semantics
better than `ReliableStream` (byte stream with no message boundaries).

```rust
pub struct NtiedConnection {
    conn: v2::Connection,
    stream: v2::DatagramStream,
    peer_id: Option<PeerId>,
}

impl NtiedConnection {
    pub async fn send(&self, data: impl Into<Vec<u8>>) -> io::Result<()> {
        self.stream.send(&data.into()).await
    }

    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        self.stream.recv().await
    }

    pub fn peer_id(&self) -> Option<&PeerId> { ... }
    pub async fn peer_public_key(&self) -> Option<v2::crypto::PublicKey> { ... }
}
```

#### Shared socket requirement

In v1, `Transport` and `ServerDiscovery` share one UDP socket: the transport's
recv loop handles both peer-to-peer packets and server protocol messages. The
server identifies clients by their UDP source address.

In v2, `ServerDiscovery::connect()` creates its **own** socket, separate from
the transport's socket. This breaks `ServerDiscovery` because:

1. The transport registers `(peer_id, transport_addr)` with the server, but
   heartbeats arrive from the discovery socket's address — the server does not
   recognize them and marks the client inactive.
2. Peers resolve each other's transport address, but the server only sees
   heartbeats from discovery addresses — stale registrations get pruned.

**Fix** (in progress): share one socket between transport and discovery.

- `ServerDiscovery::new_shared(socket, server_addr)` — accepts an external
  `Arc<UdpSocket>`, spawns only the heartbeat task (no recv loop).
- `Transport::bind_with_socket(socket, identity, discovery)` — accepts an
  external `Arc<UdpSocket>` instead of creating its own.
- `Discovery::handle_packet(data, addr) -> bool` — called by the transport's
  recv loop for every incoming packet. `ServerDiscovery` tries to parse it as
  a `ServerResponse`; returns `true` if handled, `false` to let the transport
  process it.
- `NtiedTransport::bind` creates one `UdpSocket`, passes `Arc` to both.

This approach keeps one recv loop (inside `Transport`) that demultiplexes
packets by trying the discovery handler first, then falling through to the
transport packet parser. Both sides tolerate unknown packets gracefully.

#### Identity: `v2::crypto::PrivateKey`

`ntied_crypto` (ECDSA-P256) is dropped entirely. The sole identity type is
`v2::crypto::PrivateKey` (hybrid Ed25519 + ML-DSA-65). This is a breaking
change — all stored identities must be regenerated and contacts must re-pair.

Peer addressing throughout `ntied` switches from `PublicKey` to `PeerId`
(33 bytes, hashable, comparable). `PublicKey` (1984 bytes) is still available
after authentication but is no longer used as a map key or identifier.

#### Changes in this phase

| File | Change | Status |
|------|--------|--------|
| `ntied-transport/src/v2/mod.rs` | Top-level re-exports | ✅ Done |
| `ntied-transport/src/v2/crypto/identity.rs` | `Clone` for `PrivateKey`; `to_bytes`/`from_bytes`; split `KeyPair` into `SigningKey`+`VerifyingKey` | ✅ Done |
| `ntied-transport/src/v2/discovery/traits.rs` | Add `handle_packet` to `Discovery` trait | ✅ Done |
| `ntied-transport/src/v2/discovery/server.rs` | Add `new_shared(socket, addr)` constructor | 🔧 In progress |
| `ntied-transport/src/v2/api.rs` | Add `bind_with_socket`; call `handle_packet` in recv loop | 🔧 In progress |
| `ntied/Cargo.toml` | Remove `ntied-crypto` dep | ✅ Done |
| `ntied/src/transport.rs` | New — `NtiedTransport`, `NtiedConnection` wrappers | ✅ Done (uses `ServerDiscovery::connect`, needs shared-socket update) |
| `ntied/src/lib.rs` | Add `pub mod transport` | ✅ Done |
| `config/mod.rs` | `ntied_crypto::PrivateKey` → `v2::crypto::PrivateKey`; base64 byte storage | ✅ Done |
| `models/contact.rs` | `public_key: PublicKey` → `peer_id: PeerId`; column `"peer_id"` | ✅ Done |
| `contact/listener.rs` | All callback params `PublicKey` → `PeerId` | ✅ Done |
| `contact/manager.rs` | `NtiedTransport`; `HashMap<PeerId, _>`; `get_own_peer_id()`; `on_incoming_peer_id()` | ✅ Done |
| `contact/handle.rs` | `NtiedConnection`; `PeerId`; `io::Error` | ✅ Done |
| `call/listener.rs` | All callback params `PublicKey` → `PeerId` | ✅ Done |
| `call/handle.rs` | `peer_id: PeerId`; `peer_id()` | ✅ Done |
| `call/manager.rs` | All `PublicKey` → `PeerId` | ✅ Done |
| `chat/listener.rs` | Callback params `PublicKey` → `PeerId` | ✅ Done |
| `chat/handle.rs` | `peer_id()` instead of `public_key()` | ✅ Done |
| `chat/manager.rs` | `HashMap<PeerId, _>`; DB column `"peer_id"` | ✅ Done |
| `ui/listener.rs` | All `PublicKey` → `PeerId` in listener impls | ✅ Done |
| `ui/app.rs` | `PeerId::parse` instead of `PublicKey::from_str` | ✅ Done |
| `ui/screens/*.rs` | `PeerId::parse`; `get_own_peer_id()`; `contact.peer_id` | ✅ Done |
| Tests: `contact_manager_tests.rs` | v2 types; `run_async` with 16 MB stack | ✅ Done |
| Tests: `chat_manager_tests.rs` | v2 types; `run_async` with 16 MB stack | ✅ Done |
| Tests: `models_tests.rs` | v2 types | ✅ Done |

#### Current blocker

All code compiles. Unit tests pass (`models_tests`, `v2::crypto`, `v2_api_tests`).
Integration tests (`contact_manager_tests`, `chat_manager_tests`) hit the
shared-socket problem described above — `ServerDiscovery` uses a separate UDP
socket from `Transport`, so the server doesn't recognize heartbeats and prunes
registrations.

**Next step**: finish `ServerDiscovery::new_shared` + `Transport::bind_with_socket`
+ `handle_packet` integration, then update `NtiedTransport::bind` to use
shared socket. After that, integration tests should pass.

#### Stack size note

ML-DSA-65 key generation and signing require a large call stack (~4 KB signing
key alone). The default tokio test stack (≈2–8 MB) overflows. All integration
tests use `run_async` with `thread_stack_size(16 * 1024 * 1024)` matching the
pattern from `v2_api_tests.rs`.

#### Verification

Existing integration tests (`contact_manager_tests`, `chat_manager_tests`)
must pass with the wrapper. The transport is different under the hood, but the
application behavior is identical.

---

### Phase 2 — Direct v2 API usage

Remove the compatibility wrapper. Use v2 types directly in `contact/`.
At this point `PeerId` is already the primary identifier and
`v2::crypto::PrivateKey` is the sole identity — no further identity changes.

#### Stream purpose conventions

Define well-known stream purposes for each packet family:

| Purpose | Stream type | Direction | Used for |
|---------|-------------|-----------|----------|
| `0x0001` | `ReliableStream` | Bidirectional | Contact protocol (request/accept/reject) |
| `0x0002` | `ReliableStream` | Bidirectional | Chat messages + acks |
| `0x0003` | `ReliableStream` | Bidirectional | Call signaling (start/accept/reject/end) |
| `0x0004` | `DatagramStream` | Bidirectional | Audio data |
| `0x0005` | `DatagramStream` | Bidirectional | Video data |

#### ContactHandle changes

The current `ContactHandleTask` uses a single `Connection` and multiplexes
all packet types through it via `bincode`. With v2:

1. On connection established, open streams for each purpose.
2. Each stream type gets its own send/recv loop.
3. The `select!` in `accepted_loop` reads from multiple streams instead of
   one `connection.recv()`.

**Before (v1):**
```
connection.send(bincode::serialize(&Packet::Chat(chat_packet)))
connection.send(bincode::serialize(&Packet::Call(call_packet)))
```

**After (v2):**
```
chat_stream.send(bincode::serialize(&chat_packet))
call_signal_stream.send(bincode::serialize(&call_signaling_packet))
audio_datagram.send(&encoded_audio_frame)
```

#### ContactManager changes

```
// Phase 1 wrapper
NtiedTransport::bind("0.0.0.0:0", private_key, server_addr)
transport.connect(&peer_id)
connection.peer_id().await  // Option<PeerId>

// Phase 2 direct
let socket = Arc::new(UdpSocket::bind(addr).await?);
let discovery = Arc::new(ServerDiscovery::new_shared(socket.clone(), server_addr));
v2::Transport::bind_with_socket(socket, identity, discovery)
transport.connect(&peer_id)
connection.peer_id().await          // Option<PeerId>
connection.peer_public_key().await  // Option<PublicKey> — for display/verification only
```

Contact store already uses `PeerId` as map key (changed in Phase 1).
No further identity changes needed here.

#### Changes in this phase

| File | Change |
|------|--------|
| `ntied/src/transport.rs` | Delete — wrapper no longer needed |
| `contact/manager.rs` | Use `v2::Transport`, `v2::Connection` directly |
| `contact/handle.rs` | Multiple streams per connection; separate loops per purpose |
| `call/manager.rs` | Audio send via `DatagramStream` instead of `CallPacket::AudioData` over reliable channel |
| `chat/handle.rs` | Use dedicated chat stream (purpose `0x0002`) |
| `packet/base.rs` | Remove top-level `Packet` enum — each stream carries its own packet type |
| `packet/call.rs` | Split `CallPacket` into signaling (reliable) and media (datagram) |

#### Verification

All existing tests updated. Integration test with two peers using v2 transport:
connect, exchange contacts, send chat messages, establish a call with audio.

---

### Phase 3 — Leverage v2 features

Optimizations and new capabilities enabled by v2 multiplexed streams.

#### Audio over DatagramStream

v1 sends `AudioDataPacket` as bincode over a reliable channel — head-of-line
blocking causes latency spikes on packet loss. v2 `DatagramStream` provides
reliable message-oriented delivery without ordering constraints between messages:

- Each audio frame is one datagram message
- Lost frames are retransmitted (reliable), but don't block subsequent frames
- No need for application-level sequence numbers — the datagram layer handles it

#### Separate streams per concern

| Benefit | Description |
|---------|-------------|
| No head-of-line blocking | A slow chat message ACK doesn't delay audio |
| Independent flow control | Each stream has its own window |
| Clean shutdown | Close chat stream without dropping call |
| Purpose-based routing | `accept_stream()` returns purpose — no need to parse a top-level enum |

#### Remove `Packet` top-level enum

The current `Packet` enum multiplexes three families over one channel:

```rust
enum Packet {
    Contact(ContactPacket),
    Chat(ChatPacket),
    Call(CallPacket),
}
```

With per-purpose streams, each stream carries only its own packet type.
The `Packet` enum and the dispatch logic in `accepted_loop` disappear:

```rust
// Before: one recv, parse top-level enum, dispatch
match bincode::deserialize::<Packet>(&data) {
    Ok(Packet::Chat(p)) => { ... }
    Ok(Packet::Call(p)) => { ... }
    ...
}

// After: each stream knows its type
// chat_stream task:
let p: ChatPacket = bincode::deserialize(&chat_stream.recv().await?)?;

// call_signal_stream task:
let p: CallSignalPacket = bincode::deserialize(&call_stream.recv().await?)?;

// audio_datagram task:
let frame: Vec<u8> = audio_datagram.recv().await?;
```

#### Connection re-establishment

v2 `Discovery` with `recv_connection_request` + HolePunch allows the responder
to proactively open NAT mappings. The current `ContactHandleTask` reconnection
logic (racing `outgoing_connection` vs `incoming_connection` with a timeout)
can be simplified:

- `establish_connection` just calls `transport.connect(&peer_id)`
- HolePunch is handled automatically by the transport layer
- No more manual `accept_with_holepunch` / `accept_with_full_info` branching

---

## Migration order (file by file)

### Step 0 — Prerequisites

- [x] v2 `Discovery` trait with `recv_connection_request`
- [x] v2 `ServerDiscovery` ported from v1
- [x] v2 NAT hole punching (HolePunch burst + auto-cancel)
- [x] v2 `DatagramStream` (reliable datagram)
- [x] v2 top-level re-exports (so ntied can `use ntied_transport::v2::*` cleanly)
- [ ] Shared socket: `ServerDiscovery::new_shared` + `Transport::bind_with_socket` + `Discovery::handle_packet`

### Step 1 — Identity switchover + Transport swap (Phase 1)

1. ~~Remove `ntied_crypto` dependency; add `v2::crypto` imports~~ ✅
2. ~~Update `config/mod.rs` — `v2::crypto::PrivateKey::generate()`, store hybrid keypair~~ ✅
3. ~~Update `models/contact.rs` — primary key `PeerId`, optional `PublicKey` metadata~~ ✅
4. ~~Replace `PublicKey` → `PeerId` in all listeners, handles, managers~~ ✅
5. ~~Storage migration — regenerate identity, clear contact list (breaking)~~ ✅
6. ~~Create `ntied/src/transport.rs` — `NtiedTransport` + `NtiedConnection` wrappers~~ ✅
7. ~~Update `contact/manager.rs` — use `NtiedTransport`, `HashMap<PeerId, ContactHandle>`~~ ✅
8. ~~Update `contact/handle.rs` — use `NtiedConnection`, `PeerId` for peer identity~~ ✅
9. Verify: all tests pass — **blocked on shared socket** (Step 0 prerequisite)

### Step 2 — Drop wrapper, use v2 directly (Phase 2)

1. Replace `NtiedTransport` → `v2::Transport` in `contact/manager.rs`
2. Replace `NtiedConnection` → `v2::Connection` + explicit streams in `contact/handle.rs`
3. Define stream purpose constants
4. Split `accepted_loop` into per-stream tasks
5. Delete `ntied/src/transport.rs`
6. Verify: all tests pass

### Step 3 — Audio/video over DatagramStream (Phase 3)

1. Split `CallPacket` into `CallSignalPacket` (reliable) and raw audio frames (datagram)
2. Update `call/manager.rs` — open `DatagramStream` for audio
3. Remove `AudioDataPacket` / `VideoDataPacket` from `CallPacket`
4. Verify: call tests pass

### Step 4 — Remove Packet enum

1. Each stream purpose gets its own packet type (already partially done in step 4)
2. Remove `packet/base.rs` `Packet` enum
3. Simplify dispatch logic in `contact/handle.rs`
4. Verify: all tests pass

### Step 5 — Cleanup

1. Remove v1 modules from `ntied-transport/src/` (keep only `v2/`)
2. Remove `ntied_crypto` dependency from workspace
3. Update `lib.rs` re-exports
4. Final test pass