# Transport

`ntied-transport` is the network layer. It speaks an encrypted protocol
over UDP and exposes a connection-oriented API with multiplexed
streams and channels. This document explains how it behaves and where
to look for the gory details.

If you have not read [concepts.md](concepts.md) yet, start there — the
words *Connection*, *Stream*, *Channel*, *Relay*, *Path*, and *Epoch*
all carry specific meaning here.

## Two layers in one crate

`ntied-transport` is split into a synchronous core and an async wrapper.

- **Synchronous core** (`connection::Connection`). Pure state machine.
  Owns the protocol logic — handshake, encryption, streams, channels,
  ACK and loss detection, rekeying. Does no I/O. The caller feeds it
  received bytes, asks it for bytes to send, and polls timeouts.
- **Async wrapper** (`node::Node`, `node::Connection`, etc.). Owns the
  UDP socket, spawns per-connection tasks, manages the relay pool,
  exposes a Tokio-friendly API.

Application code uses the async wrapper. The synchronous core exists
so the protocol can be tested deterministically without sockets, and
so it can be embedded in other I/O drivers later.

For the module map and byte-level wire format, read the `rustdoc`
comments next to the code in `ntied-transport/src/connection/`,
`src/wire/`, and `src/crypto/`. Those are the source of truth.

## Connection lifecycle

```
   open()/accept() ─▶ handshake ─▶ established ─▶ closing ─▶ closed
```

1. **Open / accept.** The initiator generates an ephemeral KEM keypair
   and sends an `Init` packet; the responder encapsulates and replies
   with `InitAck`. Both sides now share a secret.
2. **Handshake (authenticate).** Both sides send their `PublicKey` and
   a signature over the transcript hash, fragmented across one or
   more encrypted Data packets. Each verifies the peer's signature
   against the claimed identity, then sends an `AuthComplete` marker.
3. **Established.** Streams and channels are available; pings flow;
   data is encrypted under the current epoch's keys.
4. **Closing / closed.** Either side initiates close. Graceful close
   drains pending data and waits for ACKs before sending the close
   marker; error close skips the drain.

The handshake has a deadline (a connection that does not reach
*established* in time is closed) and the session has an idle timeout
(a connection that receives nothing for too long is closed). Exact
durations are configurable on `Config`.

## What a connection multiplexes

Inside one connection, four kinds of traffic share the wire:

- **Streams.** Reliable, ordered, byte-oriented. Each stream has its
  own send buffer, receive buffer, and flow-control window. Lost
  segments are retransmitted; in-order delivery is guaranteed per
  stream.
- **Channels.** Semi-reliable, message-oriented. Fragmentation and
  reassembly happen below the API; the application sees whole
  messages. Under buffer pressure the oldest unfinished message is
  evicted; deadlines can also expire a message. Channel open and
  close are themselves reliable.
- **Control.** Pings/pongs for RTT, window updates for flow control,
  ACKs for reliability, the close marker.
- **Key rotation.** Rekey and rekey-ack fragments live inside
  encrypted Data packets like everything else.

ID parity (initiator even, responder odd) is shared between streams
and channels so each side can allocate IDs without coordination.

## Reliability

Two complementary loss-detection mechanisms, both inspired by QUIC
(RFC 9002):

- **Gap-based.** A packet is declared lost once a few newer packets
  have been acknowledged past it.
- **Timeout-based.** A packet is declared lost if more than
  *smoothed-rtt + 4·rtt-variance* has elapsed since it was sent.

RTT is tracked with an exponential moving average and used to size the
loss-detection timeout. ACKs themselves are never ack-eliciting, which
prevents ACK feedback loops.

Each ACK frame carries a bounded number of ranges. ACK-of-ACK advances
the receiver's "floor" so the ACK-range list does not grow without
bound: when a packet that contained an ACK is itself acknowledged,
the receiver knows its sender has seen everything up to that point.

## Multi-path and relaying

A connection holds a small set of *paths* — concrete network routes
to the peer. Outbound packets are sent over the currently active path;
inbound packets are accepted from any known path and used to keep
that path's liveness fresh.

Two path kinds today:

- **Direct UDP** — packets go straight from one peer's socket to the
  other's address.
- **Relay tunnel** — packets are wrapped with a small
  `[other_peer_id]` header and sent through the relay's *tunnel
  channel*; the relay routes by that header and forwards. The wrapped
  inner packet is still encrypted end-to-end under the two peers'
  session keys.

A new connection might start on a relay path (the only one that works
through NAT), then probe a direct path in the background using
hole-punch signalling on the relay's *control channel*. If the direct
probe succeeds, it becomes active; the relay path stays as a fallback.

Because the encryption session is independent of the path (same keys,
same packet-counter sequence regardless of route), this upgrade is
seamless — no rekey, no reset of stream offsets, no disruption to the
application.

## Key rotation (rekey)

Long-lived connections rotate symmetric keys without re-handshaking.
The initiator generates a fresh KEM keypair and sends its public key
in `Rekey` frames inside the encrypted session; the responder
encapsulates and sends back `RekeyAck`; both sides derive new keys for
the next *epoch* and switch sending traffic into the new epoch.

Old-epoch keys are kept long enough for in-flight packets to decrypt,
then wiped once ACK-of-ACK confirms the peer has moved on. The epoch
field on each Data packet is two bits — enough to identify "old" vs
"new" without needing absolute synchronisation.

Forward secrecy is the goal: an attacker who recovers the keys of one
epoch cannot decrypt traffic from any other epoch.

> **Known gap.** A periodic rekey trigger is not wired up yet. The
> rekey state machine works end-to-end, but no timer currently fires
> it on its own. See [notes.md](notes.md#must-fix).

## Encryption surface

Every data-carrying packet is AEAD-encrypted with ChaCha20-Poly1305.
Keys are derived per epoch via HKDF-SHA3-256 from the KEM transcript;
the nonce binds packet counter and direction; the AAD covers the
packet header. The detailed key schedule and packet-byte layout live
in `rustdoc` next to the implementation under
`ntied-transport/src/crypto/` and `ntied-transport/src/wire/`; the
security rationale is in [security.md](security.md).

## Limits and configuration

`Config` exposes the knobs an application is most likely to touch:

- Maximum streams per direction.
- Maximum channels per direction.
- Per-stream and per-channel buffer sizes.
- Keepalive interval (or `None` to disable).
- Idle timeout and handshake timeout.

Defaults are sized for chat-grade workloads. Applications that move
larger payloads (for example, software-encoded video frames over a
channel) typically raise `channel_buf_size`.

For numeric defaults, see `Config::default()` in
`ntied-transport/src/connection/connection.rs`.

## Public API at a glance

The async surface is small enough to skim:

- `Node::bind`, `Node::bind_with_config` — open a UDP socket and a
  receive loop.
- `Node::connect(addr)` — open a direct connection.
- `Node::connect_via_relay(peer_id, relay_addr)` — open a tunnelled
  connection through a relay.
- `Node::accept()` — yield the next incoming connection.
- `Node::serve_as_relay()` — run this node as a relay server.
- `Connection::open_stream` / `accept_stream`, `open_channel` /
  `accept_channel` — multiplex within an established connection.
- `Stream::send` / `recv` / `close`, `Channel::send` / `recv` /
  `close` — the per-stream / per-channel I/O.

Examples are under `ntied-transport/tests/` (`handshake.rs`,
`streams.rs`, `relay.rs`) and `ntied-transport/examples/`.

## Known limitations and future work

Tracked in [notes.md](notes.md). The short version: no congestion
control, no periodic rekey trigger, two known retransmit edge cases.
None of those block normal use, but all are on the must-fix list
before the transport is considered production-grade.
