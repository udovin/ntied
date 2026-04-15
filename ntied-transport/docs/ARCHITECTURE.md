# Architecture

## Layers

```
┌─────────────────────────────────────────────────┐
│  node_v2                                        │
│  Async event loop, UDP socket, accept/connect   │
│  Drives connection_v2 via send/recv/on_timeout   │
├─────────────────────────────────────────────────┤
│  connection_v2::Connection                      │
│  Synchronous state machine, no I/O              │
│  ┌────────────┐ ┌──────────────┐ ┌───────────┐ │
│  │ StreamMgr  │ │ ChannelMgr   │ │ Ack/Loss  │ │
│  │ SendBuf    │ │ Fragmenter   │ │ RTT       │ │
│  │ RecvBuf    │ │ Assembler    │ │ Retransmit│ │
│  └────────────┘ └──────────────┘ └───────────┘ │
│  ┌──────────────────────────────────────┐       │
│  │ wire::frame / wire::packet           │       │
│  │ Zero-copy encode/decode              │       │
│  └──────────────────────────────────────┘       │
├─────────────────────────────────────────────────┤
│  crypto                                         │
│  Identity (Ed25519 + ML-DSA-65)                 │
│  KEM (X25519 + ML-KEM-768)                      │
│  AEAD (ChaCha20-Poly1305)                       │
└─────────────────────────────────────────────────┘
```

## Key Entities

### Connection

Stateful, synchronous protocol engine. Created via `Connection::open()` (initiator)
or `Connection::accept()` (responder). The caller is responsible for:

- Calling `send()` to produce outgoing packets
- Calling `recv()` when a packet arrives
- Calling `on_timeout()` when the timer from `timeout()` fires
- Polling `drain_updated_streams()`/`drain_updated_channels()` to discover new peer activity

Connection does **no I/O**. It writes into caller-provided buffers and reads from
caller-provided byte slices. This makes it testable without sockets.

**State machine:**

```
Initiator:  Init -> InitSent -> Authenticating -> Established -> Closing -> Closed
Responder:       SendInitAck -> Authenticating -> Established -> Closing -> Closed
```

- `Init` / `InitSent` / `SendInitAck`: KEM key exchange (1-RTT)
- `Authenticating`: both sides exchange signed identity (Ed25519 + ML-DSA-65)
- `Established`: full data path -- streams, channels, pings, rekey
- `Closing`: graceful close -- drain all data, wait for ACK, then send ConnectionClose
- `Closed`: terminal, no further I/O

### Stream

Reliable, ordered byte stream. Semantics similar to TCP or QUIC streams.

- **Bidirectional**: each stream has independent send and receive buffers
- **Implicit creation**: first `stream_write()` or received Stream frame creates the stream
- **ID parity**: initiator uses even IDs (0, 2, 4, ...), responder uses odd (1, 3, 5, ...)
- **Gap-fill**: accessing stream N implicitly creates all same-parity streams below N
- **Flow control**: per-stream receive window advertised via WindowUpdate frames
- **FIN**: either side can send FIN to signal end-of-stream
- **Cleanup**: stream removed when both sides finished (send FIN acked, recv FIN consumed)

**Limits:**
- 256 streams per direction (local and peer independently)
- Default buffer: 64 KB per stream per direction
- Exceeding the peer's stream limit is a protocol violation (ConnectionClose)

### Channel

Semi-reliable, message-oriented channel. Semantics similar to datagrams with deadlines.

- **Messages, not bytes**: send/recv whole messages; internally fragmented and reassembled
- **Deadlines**: each message has a deadline; expired messages are dropped before sending
- **Eviction**: when the buffer is full, the oldest incomplete message is evicted
- **ID parity**: same as streams -- initiator even, responder odd
- **Gap-fill**: same as streams
- **ChannelOpen**: reliable frame sent when a local channel is first created
- **ChannelClose**: reliable frame sent when closing; peer's `local_count` freed only after ACK

**Limits:**
- 256 channels per direction
- Default buffer: 64 KB per channel
- Exceeding the peer's channel limit is a protocol violation (ConnectionClose)

### Ping / Pong

Application-level latency measurement. `ping()` queues a ping; the response
updates `ping_rtt()`. No automatic keepalive at the state machine level --
the caller (node_v2) manages ping intervals.

### Rekey

In-place key rotation without closing the connection.

- Initiator generates new KEM keypair, sends public key via Rekey frames
- Responder encapsulates and responds with RekeyAck frames
- Both derive new keys for the next epoch
- Up to 4 concurrent epochs (2-bit counter)
- Collision: initiator wins the tie-break, responder yields
- Old epoch keys cleaned: N-2 immediately, N-1 deferred until ACK-of-ACK confirms

## Guarantees

### Reliability

| Primitive | Guarantee |
|-----------|-----------|
| Stream data | Reliable, ordered delivery. Lost data retransmitted. |
| Channel messages | Semi-reliable. Messages can be evicted under memory pressure or expired by deadline. Fragments of accepted messages are retransmitted. |
| ChannelOpen/Close | Reliable. Retransmitted on loss. |
| ConnectionClose | Reliable. Retransmitted on loss. |
| WindowUpdate | Reliable. Retransmitted on loss (latest value). |
| Ping/Pong | Best-effort. Lost pings are not retransmitted. Lost pongs are retransmitted. |
| Auth/Rekey | Reliable. Fragments retransmitted on loss. |

### Ordering

- Stream data is delivered in offset order (out-of-order data buffered until gaps filled)
- Channel messages are independent -- no ordering between messages
- Frames within a packet are processed in order

### Connection Close

**Graceful close** (`error_code == 0`):

1. Application calls `close(0, reason)`
2. State transitions to `Closing`
3. All pending stream/channel data continues to emit
4. After all data sent **and** all in-flight packets ACKed -> ConnectionClose frame sent
5. Peer receives ConnectionClose -> state = Closed

**Error close** (`error_code != 0`):

ConnectionClose sent immediately, no drain. Used for:
- Protocol violations (TooManyStreams, TooManyChannels)
- Application errors

### Timeouts

| Timeout | Duration | Effect |
|---------|----------|--------|
| Handshake | 10 seconds | Connection closed if not Established within this time |
| Idle | 30 seconds | Connection closed if no packets received |
| Loss detection | RTT + 4x deviation (min 50ms) | Marks in-flight packets as lost, triggers retransmission |

### Flow Control

Per-stream receive window. When the application reads data, freeing buffer space,
a WindowUpdate frame is sent to the peer to advertise the new limit.
No connection-level flow control -- each stream is independent.

### Security

- 1-RTT post-quantum key exchange (ML-KEM-768 + X25519)
- Mutual authentication via signed transcript hash (Ed25519 + ML-DSA-65)
- All data packets encrypted with ChaCha20-Poly1305
- Epoch-based key rotation with forward secrecy
- Stale epoch keys cleaned after ACK-of-ACK confirmation
- Duplicate packet detection via counter tracking

### Limits

| Resource | Limit | Enforcement |
|----------|-------|-------------|
| Streams per direction | 256 | Local: error returned to application. Peer: ConnectionClose. |
| Channels per direction | 256 | Same as streams. |
| Stream buffer | 64 KB | Flow control (WindowUpdate). |
| Channel buffer | 64 KB | Eviction of oldest message. |
| ACK ranges | 64 | Oldest ranges dropped. |
| Send burst | 32 packets | node_v2 limit per event loop iteration. |

## Module Map

```
connection_v2/
  connection.rs     State machine, public API
  ack.rs            ACK generation, loss detection, RTT estimation
  stream/
    manager.rs      StreamManager -- multiplex, gap-fill, limits
    buffer.rs       SendBuf/RecvBuf -- offset tracking, reorder, flow control
  channel/
    manager.rs      ChannelManager -- multiplex, gap-fill, limits, ChannelOpen/Close
    message.rs      MessageFragmenter/Assembler -- fragmentation, reassembly
  wire/
    frame.rs        Frame encode/decode (zero-copy)
    packet.rs       Packet encode/decode (Init, InitAck, Data)

node_v2/
  node.rs           UDP listener, packet routing
  connection.rs     Async event loop, accept/connect, notify_and_accept
  stream.rs         Async Stream wrapper (send/recv/close)
  channel.rs        Async Channel wrapper (send/recv/close)
```

## TODO

### Must Have

- **Congestion control** -- no send pacing. A fast sender can saturate the network
  and cause loss spirals. Need a CUBIC or BBR-like algorithm.
- **Rekey timer** -- rekey state machine works but nothing triggers periodic rekeying.
  Long-lived connections reuse the same keys.
- **Timeout-based loss recovery** -- `loss_detection_pending` flag is set by `on_timeout()`
  but the retransmission path has bugs (see test `bug_timeout_loss_detection_never_retransmits`).
- **Auth frame loss recovery** -- auth fragments are not tracked in `SendAckState`,
  so lost auth frames hang the handshake until timeout.
- **Configurable limits** -- max_streams, max_channels, buffer sizes as constructor parameters.

### Nice to Have

- **Connection error codes** -- define error code space (currently ad-hoc: 0=graceful, 1=too many streams, 2=too many channels)
- **Stream priority** -- currently round-robin; no way to prioritize streams
- **Idle ping** -- automatic keepalive at connection level instead of relying on node_v2
- **Connection migration** -- change peer address without re-handshaking
- **0-RTT data** -- send data with Init packet (requires cached peer keys)
