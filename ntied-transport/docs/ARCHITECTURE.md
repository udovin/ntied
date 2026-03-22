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
│   ├── reliable.rs     Reliable ordered stream: offset tracking, reorder buffer
│   ├── datagram.rs     Reliable datagram: fragmentation, reassembly
│   ├── unreliable.rs   Unreliable datagram: passthrough
│   └── manager.rs      Stream lifecycle: open, close, multiplex
│
├── packet/           Packet-level mechanisms (no I/O)
│   ├── assembler.rs    Pack frames into MTU-sized packets
│   ├── loss.rs         ACK processing, loss detection, retransmission
│   └── congestion.rs   Congestion control and send pacing
│
├── net/              I/O layer
│   ├── socket.rs       UDP socket wrapper
│   ├── router.rs       Main event loop: recv → dispatch → send
│   ├── direct.rs       Direct peer-to-peer link
│   └── relay.rs        Relay link (wraps packets in Relay envelope)
│
├── discovery/        Peer discovery
│   ├── traits.rs       Discovery trait definition
│   ├── server.rs       Centralized discovery server
│   └── dht.rs          DHT-based discovery
│
└── api.rs            Public API
```

## Layer Dependencies

```
crypto      →  (external crates only)
wire        →  crypto
session     →  crypto, wire
stream      →  wire
packet      →  wire
net         →  session, stream, packet, wire, crypto
discovery   →  net
api         →  net, discovery
```

Lower layers never import from upper layers.

---

## Session & Connection Interaction (Data Flow)

The `net/Connection` acts as a coordinator but delegates all cryptographic and state machine logic to the `session/Session` facade. `Connection` does not manage keys, epochs, or handshake fragments.

### 1. Ingress Routing (Decrypt & Dispatch)
1. `net/Connection` receives a packet (`Data` or handshakes), extracts the `counter`, and checks `packet/loss.rs` (`RecvAckState`) for replay protection.
2. `Connection` calls `decrypted_data = session.decrypt(data_packet)`.
   - *Epoch Rotation:* If the packet is valid and encrypted with `next` keys (future epoch), `Session` automatically promotes the internal key state (`next` -> `current` -> `previous`).
3. `Connection` parses frames from `decrypted_data.payload`.
4. **Dispatch:**
   - **Control Frames** (`Auth`, `Rekey`, `RekeyAck`): Sent to `session.process_incoming_frame(frame)`.
     - *Note:* The `session/` module uses its internal `FragmentCollector` to assemble these large cryptographic frames. Upon completion, `Session` automatically derives keys, checks transcript hashes, prevents duplicate requests, and returns `SessionEvent` (e.g. `SendRekeyAck(Vec<u8>)` or `KeysRotated`).
   - **Data Frames** (`StreamData`, `DatagramFragment`, `Ack`): Sent to `stream/` and `packet/` managers.
     - *Note:* User data fragmentation is handled entirely by `stream/datagram.rs`, keeping `session/` purely for cryptography.

### 2. Egress Routing (Collect & Encrypt)
1. `net/Connection` collects outgoing data frames from `stream/` and ACKs from `packet/`.
2. If `process_incoming_frame` triggered an event (e.g. `SendRekeyAck`), the outgoing frames are bundled.
3. All frames are serialized into a `DecryptedData` structure (with `receiver_session_id` and raw `payload`).
4. `Connection` calls `data_packet = session.encrypt(decrypted_data)` and sends the `Data` packet.
   - *Note:* `encrypt` will automatically increment the strictly monotonic `send_counter` and use the active `current_epoch`.

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

## Public API (target)

```rust
struct Transport { /* ... */ }

impl Transport {
    async fn bind(addr, identity: PrivateKey, discovery: impl Discovery) -> Result<Self>;
    async fn connect(&self, peer: &PeerId) -> Result<Connection>;
    async fn accept(&self) -> Result<Connection>;
    fn local_addr(&self) -> SocketAddr;
}

struct Connection { /* ... */ }

impl Connection {
    fn peer_id(&self) -> &PeerId;
    fn peer_identity(&self) -> &PublicKey;
    async fn open_stream(&self, purpose: u16) -> Result<ReliableStream>;
    async fn accept_stream(&self) -> Result<(ReliableStream, u16)>;
    async fn open_datagram(&self, purpose: u16) -> Result<DatagramChannel>;
    async fn accept_datagram(&self) -> Result<(DatagramChannel, u16)>;
    async fn close(&self) -> Result<()>;
}

struct ReliableStream { /* ... */ }

impl ReliableStream {
    async fn send(&self, data: &[u8]) -> Result<()>;
    async fn recv(&self) -> Result<Vec<u8>>;
    async fn close(&self) -> Result<()>;
}

struct DatagramChannel { /* ... */ }

impl DatagramChannel {
    async fn send(&self, message: &[u8]) -> Result<()>;
    async fn recv(&self) -> Result<Vec<u8>>;
    async fn send_unreliable(&self, data: &[u8]) -> Result<()>;
    async fn recv_unreliable(&self) -> Result<Vec<u8>>;
}
```
