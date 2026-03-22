# ntied-transport v2 — Architecture

## Module Structure

```
v2/
├── crypto/           Cryptographic primitives (no I/O, no protocol logic)
│   ├── identity.rs     Ed25519 + ML-DSA-65 hybrid identity
│   ├── kem.rs          X25519 + ML-KEM-768 hybrid KEM
│   └── aead.rs         HKDF-SHA3-256 key derivation + ChaCha20-Poly1305 AEAD
│
├── wire/             Wire format definitions + serialization (no I/O, no state)
│   ├── codec.rs        Binary Reader/Writer
│   ├── packet.rs       Outer packet types and serialization
│   └── frame.rs        Inner frame types and serialization
│
├── session/          Session state machines (no I/O)
│   ├── facade.rs       Facade `Session`: state management and event-driven frame processing
│   ├── state.rs        CryptoState: pure crypto engine (encrypt/decrypt, tri-state epoch rotation)
│   ├── handshake.rs    AuthState: Phase 2 logic (Auth assembly, signature verification with transcript hash)
│   ├── rekey.rs        RekeyState: KEM, key exchange, handling duplicates
│   └── fragment.rs     FragmentCollector: generic assembler for crypto frames
│
├── stream/           Stream management (no I/O)
│   ├── reliable.rs     ✅ Reliable ordered stream: offset tracking, reorder buffer
│   ├── manager.rs      ✅ Stream lifecycle: open, close, accept, multiplex, flow control
│   ├── datagram.rs     ⬜ Reliable datagram: fragmentation, reassembly
│   └── unreliable.rs   ⬜ Unreliable datagram: passthrough
│
├── packet/           Packet-level mechanisms (no I/O)
│   ├── loss.rs         ✅ ACK processing, loss detection, retransmission, RTT
│   └── congestion.rs   ⬜ Congestion control and send pacing
│
├── net/              Connection coordinator (no raw I/O — delegates to api.rs)
│   └── connection.rs   ✅ PeerConnection: decrypt → dispatch → collect → encrypt
│
├── discovery/        Peer discovery
│   ├── traits.rs       ✅ Discovery trait (resolve, register, recv_connection_request)
│   ├── hashmap.rs      ✅ HashMapDiscovery (in-memory, for testing)
│   ├── server.rs       ✅ ServerDiscovery (centralized, ported from v1)
│   └── dht.rs          ⬜ DHT-based discovery
│
└── api.rs            ✅ Public API: Transport, Connection, ReliableStream
```

## Layer Dependencies

```
crypto      →  (external crates only)
wire        →  crypto
session     →  crypto, wire
stream      →  wire
packet      →  wire
net         →  session, stream, packet, wire
discovery   →  crypto (PeerId only)
api         →  net, discovery, session, crypto, wire
```

Lower layers never import from upper layers.

---

## Session & Connection Interaction (Data Flow)

The `net/PeerConnection` acts as a coordinator but delegates all cryptographic and state machine logic to the `session/Session` facade. `PeerConnection` does not manage keys, epochs, or handshake fragments.

### 1. Ingress Routing (Decrypt & Dispatch)
1. `PeerConnection` receives a `Data` packet, checks `packet/loss.rs` (`RecvAckState`) for replay protection.
2. Calls `decrypted_data = session.decrypt(data_packet)`.
   - *Epoch Rotation:* If the packet's epoch matches `next`, `Session` triggers `handle_epoch_switch` to promote keys (`next` → `current` → `previous`).
3. Parses frames from `decrypted_data.payload`.
4. **Dispatch:**
   - **Control Frames** (`Auth`, `Rekey`, `RekeyAck`): Sent to `session.process_incoming_frame(frame)`.
     - `session/` uses `FragmentCollector` internally. On completion, returns `SessionEvent` (`AuthCompleted`, `SendRekeyAck`, `KeysRotated`).
   - **Data Frames** (`StreamData`, `StreamOpen`, `StreamClose`, `Ack`, etc.): Sent to `StreamManager` and `SendAckState`.

### 2. Egress Routing (Collect & Encrypt)
1. `PeerConnection::poll_packets` collects outgoing frames from `StreamManager` and ACKs from `RecvAckState`.
2. Frames are batched into MTU-sized groups.
3. Each batch is serialized into `DecryptedData` and encrypted via `session.encrypt(...)`.
4. `api.rs` sends the resulting `Data` packets over the UDP socket.

### 3. API Layer (api.rs)
1. `Transport::bind` opens a UDP socket, registers in discovery, spawns `recv_loop`.
2. `recv_loop` receives UDP datagrams, decodes packets, dispatches to `handle_key_exchange_init`, `handle_key_exchange_response`, `handle_data`, or handles `HolePunch` (cancels pending hole punch entries for source addr).
3. `recv_loop` also polls `discovery.recv_connection_request()` — on notification, sends HolePunch burst to the peer's address for NAT traversal.
4. `handle_data` feeds packets into `PeerConnection`, collects outgoing packets, sends them.
5. `flush_all` runs on a timer to send pending ACKs, retransmissions, keepalive pings, and scheduled HolePunch packets.
6. `Transport::connect` resolves `PeerId → SocketAddr` via `Discovery`, sends HolePunch + `KeyExchangeInit`, schedules remaining HolePunch burst, waits for handshake completion.
7. `Transport::accept` waits for inbound connections to become established.

### 4. NAT Hole Punching (api.rs)
Both sides send `HolePunch` packets to create NAT mappings before the handshake:
- **Initiator** (`connect`): sends first HolePunch immediately, schedules remaining burst, then sends `KeyExchangeInit`.
- **Responder** (`recv_loop`): receives `ConnectionRequest` from discovery, sends first HolePunch immediately, schedules remaining burst.
- **Burst**: 4 packets total, 150 ms apart. Managed via `HolePunchEntry` in `TransportState`, processed by `flush_all`.
- **Auto-cancel**: any packet received from the target `SocketAddr` (HolePunch, KeyExchangeInit, KeyExchangeResponse, Data) removes the entry.

---

## crypto/ — Cryptographic Primitives

### identity.rs

Long-term peer identity based on hybrid Ed25519 + ML-DSA-65 signatures.

#### Types

| Type        | Size     | Description                                    |
|-------------|----------|------------------------------------------------|
| `PrivateKey`| ~6 KB    | Ed25519 signing key + ML-DSA-65 keypair        |
| `PublicKey` | 1984 B   | Ed25519 verifying key (32 B) + ML-DSA-65 verifying key (1952 B) |
| `Signature` | 3373 B   | Ed25519 signature (64 B) + ML-DSA-65 signature (3309 B) |
| `PeerId`    | 33 B     | Type byte (0x01) + SHA3-256 hash of public key |

#### API

```rust
impl PrivateKey {
    fn generate() -> Self;
    fn public_key(&self) -> PublicKey;
    fn sign(&self, message: &[u8]) -> Signature;
}

impl PublicKey {
    fn verify(&self, message: &[u8], signature: &Signature) -> bool;
    fn peer_id(&self) -> PeerId;
    fn to_bytes(&self) -> [u8; 1984];
    fn from_bytes(bytes: &[u8; 1984]) -> Option<Self>;
}

impl Signature {
    fn to_bytes(&self) -> [u8; 3373];
    fn from_bytes(bytes: &[u8; 3373]) -> Option<Self>;
}

impl PeerId {
    fn to_bytes(&self) -> [u8; 33];
    fn from_bytes(bytes: [u8; 33]) -> Self;
    fn format(&self) -> String;           // URL-safe base64, no padding
    fn parse(s: &str) -> Option<Self>;    // from base64
}
```

#### Constants

| Constant                 | Value |
|--------------------------|-------|
| `ED25519_PUBLIC_KEY_SIZE`| 32    |
| `ED25519_SIGNATURE_SIZE` | 64    |
| `ML_DSA_PUBLIC_KEY_SIZE` | 1952  |
| `ML_DSA_SIGNATURE_SIZE`  | 3309  |
| `PUBLIC_KEY_SIZE`        | 1984  |
| `SIGNATURE_SIZE`         | 3373  |
| `PEER_ID_SIZE`           | 33    |
| `PEER_ID_TYPE_SHA3_256`  | 0x01  |

#### External crates

`ed25519-dalek`, `ml-dsa`, `sha3`, `hybrid-array`, `rand`

---

### kem.rs

Ephemeral hybrid KEM for key exchange: X25519 + ML-KEM-768.

Both peers generate an `EphemeralPrivateKey`. The initiator sends their
`EphemeralPublicKey`. The responder calls `encapsulate()` to produce
a `KemCiphertext` + `SharedSecret`. The initiator calls `decapsulate()`
to recover the same `SharedSecret`.

```
Initiator                              Responder
    │                                       │
    │  pk = initiator.public_key()          │
    │──────── EphemeralPublicKey ──────────>│
    │                                       │  (ct, ss) = responder.encapsulate(&pk)
    │<──────── KemCiphertext ──────────────│
    │                                       │
    │  ss = initiator.decapsulate(&ct)      │
    │                                       │
    │  Both have the same SharedSecret      │
```

#### Types

| Type                 | Size    | Description                                 |
|----------------------|---------|---------------------------------------------|
| `EphemeralPrivateKey`| ~3 KB   | X25519 static secret + ML-KEM-768 decapsulation key |
| `EphemeralPublicKey` | 1216 B  | X25519 public key (32 B) + ML-KEM-768 encapsulation key (1184 B) |
| `KemCiphertext`      | 1120 B  | X25519 public key (32 B) + ML-KEM-768 ciphertext (1088 B) |
| `SharedSecret`       | 64 B    | Raw key material (x25519_ss ‖ ml_kem_ss), input for HKDF |

#### API

```rust
impl EphemeralPrivateKey {
    fn generate() -> Self;
    fn public_key(&self) -> EphemeralPublicKey;
    fn encapsulate(&self, peer_pk: &EphemeralPublicKey) -> Option<(KemCiphertext, SharedSecret)>;
    fn decapsulate(&self, ct: &KemCiphertext) -> Option<SharedSecret>;
}

impl EphemeralPublicKey {
    fn to_bytes(&self) -> [u8; 1216];
    fn from_bytes(bytes: &[u8; 1216]) -> Self;
}

impl KemCiphertext {
    fn to_bytes(&self) -> [u8; 1120];
    fn from_bytes(bytes: &[u8; 1120]) -> Self;
}

impl SharedSecret {
    fn as_bytes(&self) -> &[u8; 64];
}
```

#### Constants

| Constant                  | Value |
|---------------------------|-------|
| `X25519_PUBLIC_KEY_SIZE`  | 32    |
| `ML_KEM_PUBLIC_KEY_SIZE`  | 1184  |
| `ML_KEM_CIPHERTEXT_SIZE`  | 1088  |
| `EPHEMERAL_PUBLIC_KEY_SIZE`| 1216 |
| `KEM_CIPHERTEXT_SIZE`     | 1120  |
| `SHARED_SECRET_SIZE`       | 64   |

#### External crates

`x25519-dalek`, `ml-kem`, `kem`, `rand`

---

### aead.rs

HKDF-SHA3-256 key derivation and ChaCha20-Poly1305 AEAD for encrypting Data packet payloads.

#### Types

| Type             | Description                                              |
|------------------|----------------------------------------------------------|
| `EncryptionKeys` | Pair of direction-specific keys derived from handshake   |
| `EncryptionKey`  | Single AEAD key with direction tag baked into nonce      |

#### Constants

| Constant         | Value |
|------------------|-------|
| `AEAD_KEY_SIZE`  | 32    |
| `AEAD_NONCE_SIZE`| 12    |
| `AEAD_TAG_SIZE`  | 16    |

#### API

```rust
impl EncryptionKeys {
    fn new(
        shared_secret: &SharedSecret,
        ephemeral_pk: &EphemeralPublicKey,
        kem_ciphertext: &KemCiphertext,
    ) -> Self;
    fn initiator_key(&self) -> &EncryptionKey;
    fn responder_key(&self) -> &EncryptionKey;
}

impl EncryptionKey {
    fn encrypt(&self, counter: u64, aad: &[u8], plaintext: &[u8]) -> Vec<u8>;
    fn decrypt(&self, counter: u64, aad: &[u8], ciphertext: &[u8]) -> Option<Vec<u8>>;
}
```

Key derivation:
```
transcript_hash = SHA3-256(ephemeral_pk || kem_ciphertext)
master_secret   = HKDF-Extract(salt = transcript_hash, ikm = shared_secret)
i2r_key         = HKDF-Expand(master_secret, "i2r", 32)
r2i_key         = HKDF-Expand(master_secret, "r2i", 32)
```

Nonce derivation (direction tag as defense-in-depth):
```
nonce[0..8]  = counter (little-endian u64)
nonce[8..11] = 0x000000
nonce[11]    = 0x01 (initiator) | 0x02 (responder)
```

#### External crates

`chacha20poly1305`, `hkdf`, `sha3`

---

## discovery/ — Peer Discovery

### traits.rs

```rust
pub struct ConnectionRequest {
    pub peer_addr: SocketAddr,
    pub peer_id: Option<PeerId>,
}

#[async_trait]
trait Discovery: Send + Sync {
    async fn resolve(&self, peer_id: &PeerId) -> Option<SocketAddr>;
    async fn register(&self, peer_id: PeerId, addr: SocketAddr);
    async fn recv_connection_request(&self) -> ConnectionRequest {
        std::future::pending().await   // default: never fires
    }
}
```

`Transport::bind` calls `discovery.register(local_peer_id, local_addr)` automatically.
`Transport::connect` calls `discovery.resolve(peer_id)` to obtain the target address.
`recv_loop` polls `discovery.recv_connection_request()` to receive incoming connection
notifications and trigger NAT hole punching.

The default `recv_connection_request` returns `std::future::pending()` — it never
resolves, so implementations that don't support notifications (e.g. `HashMapDiscovery`)
require no changes. In `select!`, the pending branch simply never fires.

### hashmap.rs

`HashMapDiscovery` — `RwLock<HashMap<PeerId, SocketAddr>>`. Intended for unit and integration tests.
All peers share the same `Arc<HashMapDiscovery>` instance. Uses default (no-op) `recv_connection_request`.

### server.rs

`ServerDiscovery` — communicates with a centralized signaling server over UDP (ported from v1).
Handles register, resolve, heartbeat, and incoming connection notifications.

When the server sends an `IncomingConnection` response (peer X at addr Y wants to connect),
`ServerDiscovery` pushes a `ConnectionRequest` into an internal `mpsc` channel.
`recv_connection_request` receives from that channel, waking via `Notify`.

### Planned: dht.rs

Port v1 `DhtDiscovery` (mainline DHT + STUN) to the new `Discovery` trait.

---

## Public API (implemented)

```rust
struct Transport { /* ... */ }

impl Transport {
    async fn bind(addr: SocketAddr, identity: PrivateKey, discovery: Arc<dyn Discovery>) -> io::Result<Self>;
    fn local_addr(&self) -> io::Result<SocketAddr>;
    async fn connect(&self, peer_id: &PeerId) -> io::Result<Connection>;
    async fn accept(&self) -> io::Result<Connection>;
}

struct Connection { /* ... */ }

impl Connection {
    fn session_id(&self) -> u64;
    async fn peer_public_key(&self) -> Option<PublicKey>;
    async fn peer_id(&self) -> Option<PeerId>;
    async fn is_established(&self) -> bool;
    async fn close(&self) -> io::Result<()>;
    async fn open_stream(&self, purpose: u16) -> io::Result<ReliableStream>;
    async fn accept_stream(&self) -> io::Result<(ReliableStream, u16)>;
}

struct ReliableStream { /* ... */ }

impl ReliableStream {
    fn stream_id(&self) -> u32;
    async fn send(&self, data: &[u8]) -> io::Result<()>;
    async fn recv(&self) -> io::Result<Vec<u8>>;
    async fn close(&self) -> io::Result<()>;
}
```

### Not yet implemented (target)

```rust
impl Connection {
    async fn open_datagram(&self, purpose: u16) -> io::Result<DatagramChannel>;
    async fn accept_datagram(&self) -> io::Result<(DatagramChannel, u16)>;
}

struct DatagramChannel { /* ... */ }

impl DatagramChannel {
    async fn send(&self, message: &[u8]) -> io::Result<()>;
    async fn recv(&self) -> io::Result<Vec<u8>>;
    async fn send_unreliable(&self, data: &[u8]) -> io::Result<()>;
    async fn recv_unreliable(&self) -> io::Result<Vec<u8>>;
}
```

---

## Implementation notes

### Stack size and post-quantum crypto

Post-quantum types (`EphemeralPrivateKey` ~3 KB, `PrivateKey` ~6 KB) create large async futures
in debug builds. To avoid stack overflows:

- `api.rs` Box-allocates `EphemeralPrivateKey` and `PeerConnection` before storing them.
- Integration tests use a custom `run_async` helper that spawns a thread with 16 MB stack
  and a multi-thread tokio runtime with matching `thread_stack_size`.