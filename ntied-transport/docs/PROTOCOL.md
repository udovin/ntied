# Protocol Specification

## Overview

UDP-based transport with post-quantum encryption, reliable streams,
semi-reliable message channels, and in-place key rotation.

All multi-byte integers are big-endian.

## Packets

Three packet types, distinguished by the first byte:

### Init (0x01)

Sent by initiator to begin handshake. Contains ephemeral KEM public key.

```
[type:1 = 0x01] [initiator_connection_id:8] [kem_public_key:1216]
```

Total: 1225 bytes.

### InitAck (0x02)

Sent by responder. Contains KEM ciphertext (encapsulated shared secret).

```
[type:1 = 0x02] [responder_connection_id:8] [initiator_connection_id:8] [kem_ciphertext:1120]
```

Total: 1137 bytes.

### Data (0x10..0x13)

Encrypted payload. Type byte encodes 2-bit epoch for key rotation.

```
[type:1 = 0x10 | (epoch & 0x03)] [receiver_connection_id:8] [counter:8] [ciphertext:N]
```

Header: 17 bytes. Ciphertext = AEAD(plaintext_frames, aad=header).

The `counter` is a monotonically increasing packet sequence number used for:
- AEAD nonce
- Duplicate detection
- ACK tracking
- Loss detection

## Frames

Frames are encoded inside the plaintext of Data packets. Multiple frames
per packet. Decoded via zero-copy iterator.

### Padding (0x00)

```
[type:1 = 0x00]
```

Skipped during decoding. Used to pad packets.

### ACK (0x01)

Acknowledges received packets. Not ack-eliciting (prevents ACK loops).

```
[type:1 = 0x01] [largest:8] [delay:2] [count:1] [ranges: count x (gap:8 + length:8)]
```

- `largest`: highest packet counter acknowledged
- `delay`: microseconds since `largest` was received
- `gap`: distance from previous range (or from `largest` for first range)
- `length`: number of contiguous counters in this range

### Ping (0x02) / Pong (0x03)

Latency measurement. Ping is ack-eliciting. Lost pongs are retransmitted.

```
[type:1 = 0x02/0x03] [id:4]
```

### AuthComplete (0x04)

Signals that auth verification succeeded. Sent after verifying peer's
signed identity. Both sides must send and receive AuthComplete before
transitioning to Established.

```
[type:1 = 0x04]
```

### ConnectionClose (0x05)

Closes the connection. `error_code = 0` for graceful close.

```
[type:1 = 0x05] [error_code:4] [reason_len:2] [reason:reason_len]
```

Error codes:
- `0` -- graceful close (data drained before sending)
- `1` -- too many streams
- `2` -- too many channels

### WindowUpdate (0x06)

Advertises receive window for a stream (flow control).

```
[type:1 = 0x06] [stream_id:8] [max_offset:8]
```

### ChannelClose (0x07)

Closes a channel. Reliable (retransmitted on loss).

```
[type:1 = 0x07] [channel_id:8]
```

### Stream (0x08..0x09)

Stream data. Bit 0 = FIN flag.

```
[type:1 = 0x08 | fin] [stream_id:8] [offset:8] [len:2] [data:len]
```

Header: 19 bytes.

### ChannelOpen (0x0A)

Signals a new channel was created. Reliable (retransmitted on loss).
If the peer already has the channel (data arrived first), this is a no-op.

```
[type:1 = 0x0A] [channel_id:8]
```

### Channel (0x10..0x11)

Channel message fragment. Bit 0 = FIN flag (last fragment of message).

```
[type:1 = 0x10 | fin] [channel_id:8] [message_id:8] [offset:8] [len:2] [data:len]
```

Header: 27 bytes.

### Auth (0x18..0x19)

Auth payload fragment (public_key || signature). Used during Authenticating state.

```
[type:1 = 0x18 | fin] [offset:8] [len:2] [data:len]
```

### Rekey (0x20..0x21)

Rekey KEM public key fragment. Initiates key rotation.

```
[type:1 = 0x20 | fin] [offset:8] [len:2] [data:len]
```

### RekeyAck (0x28..0x29)

Rekey KEM ciphertext fragment. Responds to Rekey.

```
[type:1 = 0x28 | fin] [offset:8] [len:2] [data:len]
```

## Handshake

```
Client                              Server
  |                                    |
  |--- Init (KEM public key) -------->|
  |                                    |
  |<------ InitAck (KEM ciphertext) --|
  |                                    |
  |  [Both derive shared secret]      |
  |  [Both derive epoch 0 keys]       |
  |                                    |
  |--- Data: Auth fragments --------->|
  |<-------- Data: Auth fragments ----|
  |                                    |
  |  [Both verify peer signature]     |
  |                                    |
  |--- Data: AuthComplete ----------->|
  |<------------ Data: AuthComplete --|
  |                                    |
  |  [State = Established]            |
```

1. Initiator generates ephemeral KEM keypair, sends public key in Init
2. Responder encapsulates, sends ciphertext in InitAck
3. Both derive shared secret and encryption keys (epoch 0)
4. Both send Auth frames: `public_key || signature(transcript_hash)`
5. Both verify peer's signature against transcript hash
6. Both send AuthComplete frame
7. When both AuthComplete sent and received -> Established

Auth payload is fragmented via MessageFragmenter (same as channel messages).
Transcript hash = SHA3-256(KEM_public_key || KEM_ciphertext).

## Encryption

- **Algorithm**: ChaCha20-Poly1305 (AEAD)
- **Key derivation**: HKDF-SHA3-256 from shared secret + KEM transcript
- **Nonce**: packet counter (unique per epoch)
- **AAD**: Data packet header (type + connection_id + counter)
- **Two keys per epoch**: initiator-to-responder and responder-to-initiator

## Key Rotation (Rekey)

```
Initiator                           Responder
  |                                    |
  |--- Rekey (new KEM public key) --->|
  |                                    |
  |<--- RekeyAck (KEM ciphertext) ----|
  |                                    |
  |  [Both derive new epoch keys]     |
  |  [Initiator switches to new epoch]|
  |                                    |
  |--- Data (new epoch) ------------->|
  |  [Responder sees new epoch,       |
  |   switches to match]              |
```

Epoch is 2-bit (0..3), wraps around. Key cleanup:
- N-2: cleaned immediately on epoch change
- N-1: cleaned after ACK-of-ACK confirms peer has moved on

Simultaneous rekey collision: connection initiator wins, responder yields.

## Loss Detection

Two mechanisms (inspired by QUIC RFC 9002):

**Gap-based**: a packet is declared lost if 3 or more newer packets
have been acknowledged.

**Timeout-based**: a packet is declared lost if
`rtt_average + 4 * rtt_deviation` has elapsed since it was sent
(minimum 50ms).

RTT estimation uses exponential weighted moving average:
- `rtt_average = 7/8 * avg + 1/8 * sample`
- `rtt_deviation = 3/4 * dev + 1/4 * |avg - sample|`

## ACK-of-ACK

Prevents unbounded growth of the receiver's ACK range list.

When sending a packet containing an ACK frame, the sender records
`(packet_counter, recv_ack_floor)`. When the peer ACKs that packet,
the receiver advances its floor to discard old ranges.

## Stream ID Assignment

- Initiator: even IDs (0, 2, 4, ...)
- Responder: odd IDs (1, 3, 5, ...)
- Same for channels

When a stream/channel with ID N is first accessed, all IDs of the same
parity from the current watermark up to N are implicitly created (gap-fill).
This prevents reorder from creating gaps that would be rejected as reuse.

A closed ID can never be reused -- the watermark only advances forward.

## Channel Lifecycle

```
Sender                              Receiver
  |                                    |
  |--- ChannelOpen (channel_id) ----->|
  |--- Channel fragments ----------->|  (may arrive before ChannelOpen)
  |<-------------- ACK --------------|
  |                                    |
  |--- ChannelClose (channel_id) ---->|
  |<-------------- ACK --------------|  (local_count decremented on ACK)
```

- ChannelOpen may arrive after data -- receiver's `get_or_create` handles both orders
- ChannelClose is reliable; sender's `local_count` decremented only when ACK received
- This prevents the sender from opening new channels before the receiver knows old ones are closed
