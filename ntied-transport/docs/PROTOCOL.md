# ntied-transport v2 — Protocol Specification

## Overview

ntied-transport v2 is a UDP-based peer-to-peer transport protocol providing:

- **Post-quantum hybrid cryptography** for key exchange and identity
- **Three channel types**: reliable streams, reliable datagrams, unreliable datagrams
- **NAT hole punching** for direct connectivity
- **Relay support** when direct connection is impossible
- **Pluggable peer discovery**

All communication happens over UDP. Every UDP datagram fits within a single MTU
(default 1200 bytes payload) — no IP-level fragmentation.

---

## Table of Contents

1. [Constants](#1-constants)
2. [Cryptographic Primitives](#2-cryptographic-primitives)
3. [Peer Identity](#3-peer-identity)
4. [Wire Format](#4-wire-format)
5. [Connection Lifecycle](#5-connection-lifecycle)
6. [Handshake Protocol](#6-handshake-protocol)
7. [Data Packets and Frames](#7-data-packets-and-frames)
8. [ACK Mechanism](#8-ack-mechanism)
9. [Channel Types](#9-channel-types)
10. [Key Rotation](#10-key-rotation)
11. [Keepalive](#11-keepalive)
12. [NAT Hole Punching](#12-nat-hole-punching)
13. [Relay Protocol](#13-relay-protocol)

---

## 1. Constants

| Name                   | Value       | Description                                  |
|------------------------|-------------|----------------------------------------------|
| `INITIAL_MTU`          | 1200 bytes  | Safe UDP payload size (accounts for VPN etc.) |
| `PACKET_OVERHEAD`      | 33 bytes    | type/epoch(1) + session_id(8) + counter(8) + tag(16) |
| `MAX_PACKET_PAYLOAD`   | 1167 bytes  | `INITIAL_MTU - PACKET_OVERHEAD`              |
| `MAX_FRAME_OVERHEAD`   | 7 bytes     | type(1) + length(2) + stream_id(4)           |
| `MAX_FRAME_DATA`       | ~1150 bytes | Payload per frame after frame header          |
| `MAX_DATAGRAM_MSG`     | 262144      | 256 KB limit for a single reliable datagram  |
| `MAX_ACK_RANGES`       | 64          | Max ranges reported in one ACK frame         |
| `PACKET_LOSS_THRESHOLD`| 3           | Packets received after gap before declaring loss |
| `DEFAULT_PING_INTERVAL`| 3 seconds   | Keepalive ping interval when idle            |
| `DEFAULT_IDLE_TIMEOUT` | 10 seconds  | Connection considered dead after no activity |

---

## 2. Cryptographic Primitives

All classical algorithms are based on **Curve25519**: X25519 for key exchange,
Ed25519 for signatures. Both use the same underlying elliptic curve in different
forms (Montgomery for ECDH, twisted Edwards for signatures).

### Key Exchange — Hybrid KEM

Combines classical and post-quantum algorithms. An attacker must break **both** to
compromise the key exchange.

| Component    | Algorithm           | Public Key | Ciphertext | Shared Secret |
|--------------|---------------------|------------|------------|---------------|
| Classical    | X25519 (Curve25519) | 32 B       | 32 B       | 32 B          |
| Post-Quantum | ML-KEM-768         | 1184 B     | 1088 B     | 32 B          |
| **Combined** |                     | **1216 B** | **1120 B** | **64 B raw**  |

The raw shared secret (x25519_ss ‖ ml_kem_ss, 64 bytes) is fed into HKDF to
derive the final session keys.

### Signatures — Hybrid Identity

Combines classical and post-quantum signature schemes. An attacker must break
**both** to forge an identity.

| Component    | Algorithm           | Public Key | Signature  |
|--------------|---------------------|------------|------------|
| Classical    | Ed25519 (Curve25519)| 32 B       | 64 B       |
| Post-Quantum | ML-DSA-65          | 1952 B     | 3309 B     |
| **Combined** |                     | **1984 B** | **3373 B** |

Both algorithms sign the same message. Verification requires both signatures to pass.

### Symmetric Encryption — AEAD

| Algorithm         | Key  | Nonce | Tag  |
|-------------------|------|-------|------|
| ChaCha20-Poly1305 | 32 B | 12 B  | 16 B |

Nonce is derived from packet counter (see [Section 7](#7-data-packets-and-frames)).
Counter is deterministic and monotonic — no random nonces, no collision risk.

### Key Derivation — HKDF-SHA3-256

All key material is derived through HKDF-SHA3-256 with domain-separated labels.

---

## 3. Peer Identity

### PublicKey (IdentityPublicKey)

The full hybrid public key (1984 bytes). Exchanged only during the encrypted
authentication phase of the handshake — never sent in plaintext.

### PeerId

A compact 33-byte identifier used as the peer's network address:

```
hash           = SHA3-256(ed25519_public_key || ml_dsa_public_key)   // 32 bytes
peer_id[0]     = 0x01                            // type byte: SHA3-256
peer_id[1..33] = hash[0..32]                     // full 32 bytes of hash
```

Total: **33 bytes** (1 type byte + 32 bytes hash).

The first byte is a **type tag** indicating the hash algorithm:

| Type byte  | Algorithm  | Status     |
|------------|------------|------------|
| 0x01       | SHA3-256   | Current    |
| 0x00       | (reserved) | Future use |
| 0x02..0xFF | (reserved) | Future use |

This enables future algorithm agility — old and new PeerIds can coexist in
the network, distinguished by the type byte.

String representation: URL-safe base64 without padding (44 characters).

Properties:
- **Compact**: 33 bytes, suitable for routing, discovery, relay addressing
- **One-way**: cannot recover public keys from PeerId (SHA3 preimage resistance)
- **Privacy**: public keys are never exposed to passive observers
- **Versioned**: type byte allows future hash algorithm migration (256 possible algorithms)
- **Quantum-resistant**: SHA3-256 provides 128-bit quantum security (Grover)

PeerId is used in: discovery, relay routing, handshake targeting, connection
identification. The actual cryptographic keys are revealed only to authenticated
peers through the encrypted channel.

---

## 4. Wire Format

Every UDP datagram begins with a 1-byte type field.

### Packet Types

| Type       | Name                | Direction              | Size      |
|------------|---------------------|------------------------|-----------|
| 0x01       | KeyExchangeInit     | Initiator → Responder  | 1258 B    |
| 0x02       | KeyExchangeResponse | Responder → Initiator  | 1137 B    |
| 0x03       | HolePunch           | Bidirectional          | 34 B      |
| 0x04       | Relay               | Via relay              | ≤ 1200 B  |
| 0x05..0x0F | (reserved)          |                        |           |
| 0x10..0xFF | Data                | Bidirectional          | ≤ 1200 B  |

The type byte for Data packets encodes the key epoch:
`epoch = type - 0x0F` (epochs 1..240). This allows the receiver to select
the correct decryption key from the plaintext header without trial decryption
and without spending an extra byte per packet.

Parsing rule:
- `type <= 0x0F` → control packet (KeyExchange, HolePunch, Relay)
- `type >= 0x10` → Data packet, `epoch = type - 0x0F`

### 4.1 KeyExchangeInit (0x01)

```
Offset  Size    Field
──────  ──────  ─────────────────────────────
0       1       type = 0x01
1       8       initiator_session_id: u64
9       33      target_peer_id: [u8; 33]
42      32      x25519_ephemeral_pk: [u8; 32]
74      1184    ml_kem_pk: [u8; 1184]
──────────────────────────────────────────────
Total: 1258 bytes
```

- `initiator_session_id`: randomly generated, identifies this session for the initiator.
- `target_peer_id`: PeerId of the intended responder.
- `x25519_ephemeral_pk`: X25519 ephemeral public key for classical ECDH.
- `ml_kem_pk`: ML-KEM-768 ephemeral public key for post-quantum KEM.

### 4.2 KeyExchangeResponse (0x02)

```
Offset  Size    Field
──────  ──────  ─────────────────────────────
0       1       type = 0x02
1       8       responder_session_id: u64
9       8       initiator_session_id: u64
17      32      x25519_ephemeral_pk: [u8; 32]
49      1088    ml_kem_ciphertext: [u8; 1088]
──────────────────────────────────────────────
Total: 1137 bytes
```

- `responder_session_id`: randomly generated, identifies this session for the responder.
- `initiator_session_id`: echoed back for routing.
- `x25519_ephemeral_pk`: responder's X25519 ephemeral public key.
- `ml_kem_ciphertext`: ML-KEM-768 ciphertext encapsulated to initiator's `ml_kem_pk`.

### 4.3 Data (0x10..0xFF)

```
Offset  Size    Field
──────  ──────  ─────────────────────────────
0       1       type = 0x10..0xFF (encodes epoch)
1       8       receiver_session_id: u64
9       8       counter: u64
17      var     encrypted_payload (frames + AEAD tag)
──────────────────────────────────────────────
Max total: INITIAL_MTU (1200 bytes)
```

- `type`: encodes the key epoch. `epoch = type - 0x0F`, range 1..240.
  Starts at 1 after handshake, increments on each rekey. Wraps 240 → 1.
- `receiver_session_id`: the peer's session ID, used for routing.
- `counter`: monotonically increasing per direction, serves as AEAD nonce.
- `encrypted_payload`: one or more frames encrypted with ChaCha20-Poly1305.

**AAD** (Additional Authenticated Data):
```
AAD = type || receiver_session_id || counter
```

The entire plaintext header (type byte with embedded epoch, session_id, counter)
is authenticated via AAD — allowing routing and key selection without decryption
while preventing tampering of any header field.

**Nonce derivation:**
```
nonce[0..8]  = counter as little-endian u64
nonce[8..11] = 0x000000
nonce[11]    = 0x01 (initiator) | 0x02 (responder)
```

Each direction uses a **separate key** and a **distinct nonce tag** (defense-in-depth),
so both sides safely start counter at 0.

### 4.4 HolePunch (0x03)

```
Offset  Size    Field
──────  ──────  ─────────────────────────────
0       1       type = 0x03
1       33      sender_peer_id: [u8; 33]
──────────────────────────────────────────────
Total: 34 bytes
```

Minimal packet for NAT traversal. No signature — authentication happens in the
handshake. The purpose is solely to create a NAT mapping.

### 4.5 Relay (0x04)

```
Offset  Size    Field
──────  ──────  ─────────────────────────────
0       1       type = 0x04
1       33      target_peer_id: [u8; 33]
34      var     inner_packet: [u8]
──────────────────────────────────────────────
Max total: INITIAL_MTU (1200 bytes)
```

Wraps any other packet type (0x01–0x04) for forwarding through a relay server.
The relay routes by `target_peer_id`. The inner packet is opaque to the relay
(encrypted end-to-end).

---

## 5. Connection Lifecycle

```
                    ┌──────────┐
                    │   Idle   │
                    └────┬─────┘
                         │ connect() or accept()
                         ▼
                ┌────────────────┐
                │  KeyExchange   │  Phase 1: ephemeral key exchange
                │   (1 RTT)      │  (KeyExchangeInit ↔ KeyExchangeResponse)
                └───────┬────────┘
                        │ session keys derived
                        ▼
                ┌────────────────┐
                │ Authenticating │  Phase 2: encrypted identity exchange
                │   (1 RTT)      │  (Auth fragments over encrypted channel)
                └───────┬────────┘
                        │ both identities verified
                        ▼
                ┌────────────────┐
                │  Established   │  Data, streams, datagrams
                └───────┬────────┘
                        │ idle timeout / error / close
                        ▼
                ┌────────────────┐
                │    Closed      │
                └────────────────┘
```

Data frames (streams, datagrams) are only permitted in the **Established** state.
During **Authenticating**, only Auth and Ack frames are permitted.

---

## 6. Handshake Protocol

### Phase 1 — Key Exchange (1 RTT)

```
Initiator                                     Responder
    │                                              │
    │── KeyExchangeInit (1258 B) ─────────────────>│
    │   initiator_session_id                       │
    │   target_peer_id                             │
    │   x25519_ephemeral_pk                        │
    │   ml_kem_pk                                  │
    │                                              │
    │<── KeyExchangeResponse (1137 B) ────────────-│
    │    responder_session_id                      │
    │    initiator_session_id                      │
    │    x25519_ephemeral_pk                       │
    │    ml_kem_ciphertext                         │
    │                                              │
    ├══ Both sides derive session keys ════════════┤
```

**Key derivation:**

```
x25519_ss     = X25519(our_ephemeral_sk, peer_ephemeral_pk)
ml_kem_ss     = ML-KEM-768.Decaps(our_kem_sk, peer_kem_ct)   [initiator]
              = ML-KEM-768.Encaps(peer_kem_pk) → (ct, ss)     [responder]

shared_secret = x25519_ss || ml_kem_ss   (64 bytes)

transcript_hash = SHA3-256(initiator_ephemeral_pk || kem_ciphertext)

master_secret = HKDF-Extract(
    salt = transcript_hash,
    ikm  = shared_secret
)

i2r_key       = HKDF-Expand(master_secret, "i2r", 32)
r2i_key       = HKDF-Expand(master_secret, "r2i", 32)
```

- `i2r_key`: initiator encrypts with this, responder decrypts with this.
- `r2i_key`: responder encrypts with this, initiator decrypts with this.

Separate keys per direction ensure counter=0 is safe for both sides simultaneously.

**Retransmission**: If the initiator doesn't receive KeyExchangeResponse within a
timeout, it retransmits KeyExchangeInit (same content, same session_id). The
responder treats duplicate inits idempotently.

### Phase 2 — Authentication (over encrypted channel)

Once session keys are derived, both sides simultaneously send their identity proof
as encrypted Data packets containing Auth frames.

```
Initiator                                     Responder
    │                                              │
    │══ Encrypted channel active (unauthenticated) │
    │                                              │
    │── Data[Auth 1/5] ──────────────────────────>│
    │── Data[Auth 2/5] ──────────────────────────>│
    │── Data[Auth 3/5] ──────────────────────────>│
    │── Data[Auth 4/5] ──────────────────────────>│
    │── Data[Auth 5/5] ──────────────────────────>│
    │                                              │
    │<── Data[Auth 1/5] ─────────────────────────-│
    │<── Data[Auth 2/5] ─────────────────────────-│
    │<── Data[Auth 3/5] ─────────────────────────-│
    │<── Data[Auth 4/5] ─────────────────────────-│
    │<── Data[Auth 5/5] ─────────────────────────-│
    │                                              │
    ├══ Verify identity ═══════════════════════════┤
    │   1. Reassemble IdentityPublicKey            │
    │   2. PeerId(identity_pk) == target_peer_id?  │
    │   3. ed25519.verify(transcript, signature)?  │
    │   4. ml_dsa.verify(transcript, signature)?   │
    │                                              │
    │── Data[AuthComplete] ──────────────────────>│
    │<── Data[AuthComplete] ─────────────────────-│
    │                                              │
    │══ Connection ESTABLISHED ════════════════════│
```

**Auth payload** (plaintext before encryption):

```
Offset  Size    Field
──────  ──────  ─────────────────────────────
0       32      ed25519_public_key
32      1952    ml_dsa_public_key
1984    64      ed25519_signature
2048    3309    ml_dsa_signature
──────────────────────────────────────────────
Total: 5357 bytes → 5 fragments × ~1100 B
```

Both signatures sign the same transcript:
```
signature_input = transcript_hash || "ntied v2 auth"
```

where `transcript_hash = SHA-256(KeyExchangeInit_bytes || KeyExchangeResponse_bytes)`.

Auth fragments use the reliable datagram mechanism (Section 9.2) on reserved
stream_id=0, ensuring retransmission of lost fragments via the standard ACK
mechanism.

---

## 7. Data Packets and Frames

### Packet Structure

Every Data packet carries one or more **frames** in its encrypted payload:

```
┌──────────────────────── UDP Datagram ──────────────────────────┐
│ type/epoch(1) │ session_id(8) │ counter(8) │ encrypted_payload │
│               │               │            │ ┌───────────────┐ │
│  Header (plaintext, authenticated)         │ │ Frame │ Frame │ │
│                                            │ ├───────────────┤ │
│         AAD for AEAD ─────────────────────>│ │ AEAD tag(16B) │ │
│                                            │ └───────────────┘ │
└───────────────────────────────────────────────────────────────┘
```

The type byte doubles as the epoch identifier for Data packets
(`epoch = type - 0x0F`), keeping the header at 17 bytes (no separate epoch field).

### Frame Encoding

Frames are concatenated inside the encrypted payload. Each frame:

```
Offset  Size    Field
──────  ──────  ─────────────────
0       1       frame_type: u8
1       2       frame_length: u16 (length of frame_data)
3       var     frame_data: [u8; frame_length]
```

The receiver reads frames sequentially until the payload is exhausted.

Multiple frames per packet enables batching: ACK + WindowUpdate + StreamData
in a single UDP datagram.

### Frame Types

| Type | Name             | Ack-Eliciting | Description                        |
|------|------------------|:---:|--------------------------------------------|
| 0x01 | Ack              | No  | Packet acknowledgment ranges               |
| 0x02 | Ping             | Yes | Keepalive probe                            |
| 0x03 | Pong             | No  | Keepalive response                         |
| 0x04 | StreamOpen       | Yes | Open a new stream                          |
| 0x05 | StreamData       | Yes | Reliable stream data with byte offset      |
| 0x06 | StreamClose      | Yes | Graceful stream close                      |
| 0x07 | StreamReset      | Yes | Abrupt stream termination                  |
| 0x08 | WindowUpdate     | No  | Flow control window update                 |
| 0x09 | DatagramFragment | Yes | Fragment of a reliable datagram message     |
| 0x0A | Datagram         | Yes | Unreliable single-packet datagram          |
| 0x0B | Auth             | Yes | Handshake identity fragment                |
| 0x0C | AuthComplete     | Yes | Handshake identity verified                |
| 0x0D | Rekey            | Yes | Key rotation initiation                    |
| 0x0E | RekeyAck         | Yes | Key rotation acknowledgment                |
| 0x0F | ConnectionClose  | Yes | Graceful connection shutdown                |

**Ack-eliciting**: if a packet contains at least one ack-eliciting frame, the
receiver must send an ACK. Packets with only non-ack-eliciting frames (e.g. a
pure ACK packet) do not trigger an ACK response — preventing infinite loops.

### Frame Definitions

#### 0x01 — Ack

```
largest_ack: u64            Highest packet counter received
ack_delay: u16              Microseconds since receiving largest_ack
range_count: u8             Number of ACK ranges (max MAX_ACK_RANGES)
ranges: [AckRange]          Received ranges, newest first

AckRange:
  gap: u64                  Number of missing counters before this range
  length: u64               Number of consecutive counters in this range
```

See [Section 8](#8-ack-mechanism) for the full ACK algorithm.

#### 0x02 — Ping

```
ping_id: u32                Identifier echoed back in Pong
```

#### 0x03 — Pong

```
ping_id: u32                Echoed from the Ping
```

#### 0x04 — StreamOpen

```
stream_id: u32              Unique stream identifier
stream_type: u8             0x01 = ReliableOrdered
                            0x02 = ReliableDatagram
                            0x03 = Unreliable
purpose: u16                Application-defined stream purpose
```

Stream IDs are allocated by each side from non-overlapping spaces:
- Initiator opens odd stream IDs: 1, 3, 5, ...
- Responder opens even stream IDs: 2, 4, 6, ...
- Stream ID 0 is reserved for handshake/control.

All streams are bidirectional — both sides can send and receive data.
The `purpose` field is opaque to the transport layer; it is passed through
to the application on `accept_stream()` so the receiver can route the stream
to the appropriate handler without parsing application data.

Multiple streams with the same `purpose` are permitted. If both sides
simultaneously open streams with the same purpose (simultaneous open),
the application is responsible for resolving the conflict.

#### 0x05 — StreamData

```
stream_id: u32              Target stream
offset: u64                 Byte offset within the stream
fin: u8                     0x00 = more data, 0x01 = final segment
data: [u8]                  Payload bytes (remaining frame_length)
```

#### 0x06 — StreamClose

```
stream_id: u32              Stream to close gracefully
```

#### 0x07 — StreamReset

```
stream_id: u32              Stream to reset
error_code: u32             Application-defined error code
```

#### 0x08 — WindowUpdate

```
stream_id: u32              Target stream (0 = connection-level)
max_offset: u64             New maximum byte offset the sender may send to
```

#### 0x09 — DatagramFragment

```
stream_id: u32              Target datagram channel
message_id: u32             Identifies which message this fragment belongs to
fragment_index: u16         This fragment's index (0-based)
fragment_total: u16         Total number of fragments in the message
data: [u8]                  Fragment payload (remaining frame_length)
```

All fragments of a message share the same `(stream_id, message_id)`.
The receiver buffers fragments and delivers the complete message when all
`fragment_total` fragments are received.

#### 0x0A — Datagram (Unreliable)

```
stream_id: u32              Target datagram channel
data: [u8]                  Message payload (remaining frame_length)
```

Must fit in a single frame within a single packet. No fragmentation, no
retransmission. If the packet is lost, the datagram is lost.

#### 0x0B — Auth

```
fragment_index: u8          Fragment index (0-based)
fragment_total: u8          Total fragments
data: [u8]                  Fragment of auth payload
```

Used during Phase 2 of the handshake on reserved stream_id=0.

#### 0x0C — AuthComplete

```
(empty — no fields)
```

Sent after all auth fragments received and identity verified.

#### 0x0D — Rekey

```
x25519_ephemeral_pk: [u8; 32]
ml_kem_pk: [u8; 1184]
signature: [u8]             Hybrid signature over (x25519_pk || ml_kem_pk)
```

Due to size (~3600 B), the Rekey frame is sent as multiple Data packets.
It uses the reliable delivery mechanism (retransmitted if lost).

#### 0x0E — RekeyAck

```
x25519_ephemeral_pk: [u8; 32]
ml_kem_ciphertext: [u8; 1088]
signature: [u8]             Hybrid signature over (x25519_pk || ml_kem_ct)
```

#### 0x0F — ConnectionClose

```
error_code: u32             0 = normal close, nonzero = error
reason_length: u16
reason: [u8]                Optional human-readable reason (UTF-8)
```

---

## 8. ACK Mechanism

### Overview

ACK operates at the **packet** level — acknowledging packet counters, not
individual frames. One ACK frame covers all received packets regardless of
their content.

### Receiver State

The receiver maintains a set of received packet counters as sorted, non-overlapping
ranges:

```
struct RecvAckState {
    floor: u64,                   // Counters below this are forgotten
    ranges: Vec<(u64, u64)>,      // Sorted ranges of received counters above floor
    largest: u64,                 // Highest counter received
    largest_recv_time: Instant,   // When largest was received
}
```

**Operations:**

1. **Receive packet with counter N**:
   - If `N < floor` → reject (anti-replay)
   - If `N` is already in ranges → reject (duplicate / replay)
   - Insert `N` into ranges, merging adjacent ranges

2. **Generate ACK frame**:
   - Report `largest`, `ack_delay`, and up to `MAX_ACK_RANGES` most recent ranges
   - Oldest ranges may be omitted if there are too many

3. **Advance floor** (on receiving peer's ACK of our ACK):
   - When we learn that the peer received our ACK (the peer's ACK covers
     the counter of our packet that contained an ACK), we advance `floor`
     to the `largest` value that was in our acknowledged ACK
   - All ranges below the new floor are discarded

### Sender State

The sender tracks which frames were in each sent packet:

```
struct SendAckState {
    next_counter: u64,
    sent_packets: BTreeMap<u64, SentPacket>,
    largest_acked: u64,           // Highest counter acknowledged by peer
}

struct SentPacket {
    counter: u64,
    sent_at: Instant,
    frames: Vec<Frame>,           // Frames contained in this packet
    ack_eliciting: bool,
}
```

### Loss Detection

When the sender receives an ACK from the peer:

1. **Mark acknowledged packets**: for each counter in the ACK ranges, mark the
   corresponding SentPacket as acknowledged. Remove it from tracking.

2. **Detect losses by packet threshold**: for each unacknowledged packet with
   counter `C`, if `PACKET_LOSS_THRESHOLD` (3) or more packets with counter > C
   have been acknowledged → declare packet C as lost.

3. **Detect losses by timeout**: for each unacknowledged packet, if
   `now - sent_at > RTO` → declare packet as lost.
   `RTO = smoothed_rtt * 1.5` (updated from ACK round-trip measurements).

4. **Retransmit**: extract frames from each lost packet and return them to
   the send queue. They will be packed into **new** packets with **new** counters,
   ensuring nonce uniqueness.

5. **Clean up**: remove acknowledged and retransmitted entries from `sent_packets`.

### ACK Floor Advancement (Trimming)

```
    A (data sender)                     B (data receiver)
    │                                    │
    │── P1,P2,P3,P5,P6 ───────────────>│  B.ranges = [(1,3),(5,6)]
    │                                    │  B.floor = 0
    │<── B's P①: [Ack{largest=6}] ─────│
    │                                    │
    │── P7: [Data, Ack{largest=①}] ───>│  B sees: A received B's P①
    │                                    │  B.floor advances to 6
    │                                    │  B.ranges = [(7,7)]
    │                                    │
    │<── B's P②: [Ack{largest=7}] ────-│  Small ACK: 1 range
    │                                    │
    │── P8: [Data, Ack{largest=②}] ───>│  B.floor advances to 7
    │                                    │  B.ranges = [(8,8)]
```

This keeps the ACK frame small (only recent unconfirmed ranges) and the
receiver's memory usage constant O(congestion_window).

### When to Send ACK

| Trigger                            | Action                             |
|------------------------------------|------------------------------------|
| Received ≥ 2 ack-eliciting packets | Send ACK immediately               |
| Gap detected (missing counter)     | Send ACK immediately               |
| 25 ms since last ack-eliciting     | Send ACK (delayed ACK timer)       |
| Sending a data packet              | Piggyback ACK frame in same packet |

ACK frames are **not** ack-eliciting. A packet containing only ACK (and/or Pong)
does not trigger an ACK from the receiver.

### RTT Measurement

```
On sending packet with counter C:
    record send_time[C] = now

On receiving ACK with largest_ack = C:
    if send_time[C] exists:
        rtt_sample = now - send_time[C] - ack_delay
        if first sample:
            srtt = rtt_sample
            rttvar = rtt_sample / 2
        else:
            rttvar = 0.75 * rttvar + 0.25 * |srtt - rtt_sample|
            srtt = 0.875 * srtt + 0.125 * rtt_sample
        rto = srtt + 4 * rttvar
        rto = max(rto, 50ms)   // minimum RTO
```

---

## 9. Channel Types

### 9.1 Reliable Ordered Stream (TCP-like)

A bidirectional byte stream with guaranteed in-order delivery.

**Opening**: either side sends `StreamOpen { stream_id, stream_type=ReliableOrdered, purpose }`.

**Sending data**: the sender splits data into `StreamData` frames with byte offsets:

```
StreamData { stream_id=1, offset=0,    data=[1100 bytes] }
StreamData { stream_id=1, offset=1100, data=[1100 bytes] }
StreamData { stream_id=1, offset=2200, data=[500 bytes], fin=true }
```

**Receiving data**: the receiver buffers frames by offset and delivers bytes
to the application in order. Out-of-order frames are buffered until the gap
is filled.

**Flow control**: `WindowUpdate` frames advertise how much data the receiver
is willing to accept. The sender must not send beyond the receiver's window.

```
Receiver: WindowUpdate { stream_id=1, max_offset=8192 }
  → Sender may send bytes with offset < 8192

Receiver consumed data, has room:
Receiver: WindowUpdate { stream_id=1, max_offset=16384 }
  → Sender window extends
```

**Retransmission**: handled by the packet-level ACK mechanism. When a packet
carrying a StreamData frame is declared lost, that StreamData frame is
requeued for sending in a new packet.

**Closing**:
- Graceful: sender sets `fin=true` on the last StreamData, then sends `StreamClose`
- Abrupt: either side sends `StreamReset { error_code }`

**Duplicate handling**: if a StreamData frame arrives with an offset range the
receiver already has, it is silently ignored. Deduplication is by
`(stream_id, offset)`, not by packet counter.

### 9.2 Reliable Datagram (Fragmented Message)

Message-oriented delivery with guaranteed completeness. Messages may be large
(up to `MAX_DATAGRAM_MSG` = 256 KB). No ordering guarantee between different
messages.

**Opening**: `StreamOpen { stream_id, stream_type=ReliableDatagram, purpose }`.

**Sending a message**:

1. Split message into fragments of ≤ `MAX_FRAME_DATA` bytes each.
2. Assign a `message_id` (incrementing per stream).
3. Send each fragment as a `DatagramFragment` frame.

```
32 KB message → 28 fragments:
  DatagramFragment { stream_id=2, message_id=1, index=0,  total=28, data=[1150 B] }
  DatagramFragment { stream_id=2, message_id=1, index=1,  total=28, data=[1150 B] }
  ...
  DatagramFragment { stream_id=2, message_id=1, index=27, total=28, data=[700 B] }
```

**Receiving**:
1. Buffer fragments by `(stream_id, message_id, index)`.
2. When all `total` fragments for a `message_id` are received, reassemble and
   deliver the complete message to the application.
3. Different `message_id`s are delivered independently in any order.

**Retransmission**: same packet-level ACK mechanism. Lost fragments are
retransmitted in new packets.

**Duplicate handling**: if a fragment with the same `(stream_id, message_id, index)`
arrives again, it is silently ignored.

**Timeout**: if a message cannot be completed within a configurable timeout
(e.g. 30 seconds), remaining fragments are discarded and the message is
considered lost. The application is notified.

### 9.3 Unreliable Datagram (Fire-and-Forget)

Single-packet messages with no guarantees. Suitable for real-time data where
stale data is worse than missing data.

**Opening**: `StreamOpen { stream_id, stream_type=Unreliable, purpose }`.

**Sending**: one `Datagram` frame per message. Must fit in a single frame
within a single packet.

```
Datagram { stream_id=3, data=[up to ~1150 bytes] }
```

**No fragmentation**: if the message exceeds frame capacity, the send fails.
**No retransmission**: the sender does not track unreliable datagrams.
**No ordering**: messages may arrive in any order or not at all.

### Channel Comparison

| Property           | Reliable Stream | Reliable Datagram | Unreliable Datagram |
|--------------------|:---:|:---:|:---:|
| Unit               | Byte stream     | Message           | Message             |
| Max size           | Unlimited       | 256 KB            | ~1150 bytes         |
| Fragmentation      | ✓ (by offset)   | ✓ (by msg/index)  | ✗                   |
| Retransmission     | ✓               | ✓                 | ✗                   |
| Ordering           | ✓ (in-stream)   | ✗ (between msgs)  | ✗                   |
| Flow control       | ✓               | ✗                 | ✗                   |
| Duplicate handling | By offset       | By msg_id+index   | N/A                 |

---

## 10. Key Rotation

Periodic key rotation provides forward secrecy for the session. After rotation,
compromise of old keys cannot decrypt new traffic.

### Trigger

Key rotation is initiated by either side after a configurable interval (default
15 minutes) or after a configurable number of packets.

### Protocol

```
Initiator                                     Responder
    │                                              │
    │── Data[Rekey] ─────────────────────────────>│
    │   x25519_ephemeral_pk (new)                  │
    │   ml_kem_pk (new)                            │
    │   signature(x25519_pk || ml_kem_pk)          │
    │                                              │
    │<── Data[RekeyAck] ────────────────────────-─│
    │    x25519_ephemeral_pk (new)                 │
    │    ml_kem_ciphertext (to initiator's ml_kem) │
    │    signature(x25519_pk || ml_kem_ct)         │
    │                                              │
    ├══ Both derive new session keys ══════════════┤
    │   new_master = HKDF(old_master || new_x25519_ss || new_ml_kem_ss)
    │   new_i2r = HKDF-Expand(new_master, "i2r", 32)
    │   new_r2i = HKDF-Expand(new_master, "r2i", 32)
    │                                              │
    │══ Switch to new keys, reset counters ════════│
```

Due to the large size of hybrid signatures, Rekey and RekeyAck frames may
span multiple Data packets. They use the standard ACK mechanism for reliable
delivery.

### Key Transition

After both sides have computed new keys:

1. The **sender** increments the epoch (encoded in the type byte) and switches
   to new keys immediately for outgoing packets.
2. The **receiver** accepts packets with both old and new epoch values for a
   grace period (e.g. 5 seconds), selecting the correct key by the epoch
   encoded in the type byte — no trial decryption needed.
3. After the grace period, old keys are discarded (zeroized) and packets
   with the old epoch are rejected.

### Simultaneous Rotation

If both sides initiate rotation simultaneously, the side with the lower
`session_id` wins. The other side discards its Rekey and processes the
peer's Rekey as responder (sends RekeyAck).

---

## 11. Keepalive

When no data is being exchanged:

1. After `PING_INTERVAL` (3s) of silence, send a `Ping { ping_id }` frame.
2. Receiver responds with `Pong { ping_id }` (plus an ACK since Ping is
   ack-eliciting).
3. The Pong+ACK packet is **not** ack-eliciting, so the chain stops.
4. If no packet (data, pong, or anything) is received within `IDLE_TIMEOUT` (10s),
   the connection is considered dead and closed.

Ping/Pong also serves RTT measurement when no data packets are available.

---

## 12. NAT Hole Punching

NAT hole punching uses the HolePunch packet (0x03) to create NAT mappings
before the handshake begins.

### Flow (with Discovery Server)

```
    Initiator           Discovery Server          Responder
        │                      │                      │
        │── "connect to R" ──>│                      │
        │                      │── "I wants to       │
        │                      │   connect to you" ──>│
        │<── "R is at addr" ──│                      │
        │                      │<── "I is at addr" ──│
        │                                             │
        │── HolePunch ───────────────────────────────>│  (may be dropped by NAT)
        │<── HolePunch ──────────────────────────────-│  (may be dropped by NAT)
        │── HolePunch ───────────────────────────────>│  (NAT mapping created)
        │<── HolePunch ──────────────────────────────-│  (NAT mapping created)
        │                                             │
        │── KeyExchangeInit ─────────────────────────>│
        │<── KeyExchangeResponse ────────────────────-│
        │                                             │
        │══ Continue handshake Phase 2 ═══════════════│
```

### Flow (with DHT Discovery)

When Discovery does not provide the peer's public key or connection ID, the
responder sends HolePunch packets while waiting for the initiator's
KeyExchangeInit.

```
    Initiator                                 Responder
        │                                         │
        │── HolePunch ──────────────────────────>│
        │<── HolePunch ─────────────────────────-│
        │                                         │
        │── KeyExchangeInit ───────────────────>│
        │   (responder learns initiator identity  │
        │    from target_peer_id in init)          │
        │<── KeyExchangeResponse ──────────────-│
        │                                         │
```

HolePunch packets are unauthenticated (just PeerId, 34 bytes). This is
acceptable because:
- Their only purpose is to create NAT mappings
- They carry no sensitive data
- Authentication happens in the handshake
- An attacker sending fake HolePunch packets can only create useless NAT mappings

---

## 13. Relay Protocol

When direct connection is impossible (symmetric NAT, restrictive firewall),
peers communicate through a relay server.

### How It Works

1. Both peers register with a relay server (using Discovery).
2. All packets are wrapped in a Relay envelope (0x04) with the
   `target_peer_id` for routing.
3. The relay server forwards the inner packet to the target peer.
4. Inner packets are encrypted end-to-end — the relay sees only PeerIds
   and opaque ciphertext.

### Relay Packet Format

```
┌─────┬────────────────────┬───────────────────────────────────┐
│0x04 │ target_peer_id(33) │ inner_packet (any type 0x01-0x04) │
└─────┴────────────────────┴───────────────────────────────────┘
```

### Size Constraint

Inner packet + relay overhead (34 bytes) must fit within `INITIAL_MTU`:
- Max inner packet size = 1200 - 34 = 1166 bytes
- This reduces `MAX_PACKET_PAYLOAD` for relayed connections

The transport layer adjusts frame sizes automatically when using a relay link.

### Relay Server Requirements

The relay server must:
- Track registered PeerIds and their source addresses
- Forward Relay packets to the target by PeerId
- Drop packets for unknown PeerIds
- NOT inspect or modify inner packets (they are encrypted)

The relay server **cannot**:
- Read message contents (end-to-end encrypted)
- Forge messages (AEAD authentication)
- Impersonate peers (identity verification in handshake)
