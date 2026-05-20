# Concepts

The vocabulary used across the codebase. These are the load-bearing
abstractions — knowing what each one represents (and what it
deliberately is *not*) is enough to navigate the rest of the docs and
the source.

## Identity

A long-lived cryptographic identity. Two layers, used together:

- A classical signing keypair (Ed25519).
- A post-quantum signing keypair (ML-DSA-65).

A `PrivateKey` holds both halves and is the only thing needed to act as
a peer; a `PublicKey` is what other peers verify against; a `Signature`
is the concatenation of one signature from each scheme. A signature is
accepted only if **both** halves verify (logical AND), so an attacker
must break both schemes to forge.

Identity is generated locally and never leaves the device. There is no
central authority that issues identities.

## PeerId

The hash-based handle for an identity. It is `SHA3-256(public_key)` plus
a one-byte hash-type tag, fixed at 33 bytes.

- Stable for the lifetime of an identity.
- Independent of network address — moving between networks does not
  change a peer's PeerId.
- Used as the routing key in tunnel headers when traffic goes through
  a relay.

The full `PublicKey` is exchanged during the handshake; PeerId is
derived from it and verified to match what the peer claims.

## Node

The asynchronous entry point. A `Node` binds one UDP socket, owns an
identity, and demultiplexes incoming packets to per-peer connections.
Each process typically has exactly one `Node`.

A node has three modes of use, not mutually exclusive:

- **Client.** Call `connect(addr)` to open a direct connection
  (bootstrap primitive — the answering peer's identity is whatever
  it authenticates as); `connect_direct_peer(addr, peer_id)` to also
  require a specific identity; `connect_relay_peer(relay_addr,
  peer_id)` to open one tunnelled through a relay (with the same
  identity check); or `connect_peer(peer_id)` to look up routes in
  the DHT and try direct + via-relay automatically.
- **Server-side accept.** `accept()` returns each new incoming
  connection (direct or relayed).
- **Relay server.** `serve_as_relay()` runs the node as a public relay,
  multiplexing tunnel and control traffic between its registered
  clients.

## Connection

A live, mutually authenticated session with a single remote peer.
Bidirectional, encrypted, multiplexed.

A connection carries:

- **Streams** — reliable, ordered byte streams (TCP-like).
- **Channels** — semi-reliable, message-oriented (datagram-with-deadline).
- **Liveness signals** — keepalive pings, idle/handshake timeouts.
- **Key rotation** — in-place rekey without tearing down the session.

A connection is not tied to a single network path: it can switch
between direct UDP and relay tunnelling without re-handshaking. See
[Path](#path) below.

The state-machine layer (`connection::Connection`) is **synchronous and
I/O-free**: the caller hands it bytes and the current time, and it
hands back bytes to send. The async `Node`-level `Connection` is the
wrapper that owns the actual sockets and tasks.

## Stream

A reliable, ordered, bidirectional byte stream within a connection.
Semantics are TCP-like:

- Either side can write; data arrives in order, with gaps refilled by
  retransmission.
- Either side can send FIN to half-close.
- Streams are independent of one another — head-of-line blocking on one
  stream does not stall others.
- Each direction has its own flow-control window.

Stream IDs are assigned per-connection. By convention, the connection
initiator uses even IDs and the responder uses odd IDs, so each side
can mint new stream IDs without coordination.

## Channel

A semi-reliable, message-oriented duct within a connection. Where a
stream is a byte pipe, a channel is a sequence of whole messages — used
for things like media frames where a late message is worth less than a
prompt one.

- Each `send(bytes)` becomes one message; the receiver gets it whole or
  not at all (fragmentation and reassembly are hidden).
- The send buffer is bounded; under pressure, the oldest unfinished
  message is dropped to make room. Messages can also expire by
  deadline.
- Fragments of an accepted message are retransmitted on loss, but a
  dropped message is not resurrected.
- Open and close are reliable (the peer's view of "this channel
  exists" cannot be out of sync with the sender's).

ID parity convention is the same as streams.

## Relay

A peer that forwards traffic between two other peers that cannot reach
each other directly (typically both behind NAT).

A relay has two roles in the wire protocol, both running over a normal
ntied connection between client and relay:

- **Tunnel channel** — every message is `[dest_peer_id | inner_packet]`
  on the way to the relay and `[src_peer_id | inner_packet]` on the
  way back. The relay routes by the PeerId header and never inspects
  `inner_packet`.
- **Control channel** — hole-punch signalling
  (`HolePunchRequest`/`HolePunchNotify`). Used to learn the public
  address of the peer on the other side of the relay so a direct
  upgrade can be attempted.

The `inner_packet` is encrypted under the **end-to-end** session keys
of the two peers, not the relay's keys. The relay sees PeerIds and
ciphertext, nothing else.

In code, the relay-server logic and the relay-client logic both live in
`ntied-transport`. `ntied-server` is the binary that runs the
server-side mode.

## Path

The network route a connection currently uses. A connection holds a
list of paths and picks one as active:

- **Direct** — UDP straight to a known peer address.
- **Tunnel** — through a relay's tunnel channel.

Paths have a small state machine (probing / active / idle / failing).
A new path starts in probing; it becomes active once a valid packet
arrives over it; it is demoted to idle if a better path is available.
Failing paths are eventually dropped.

The crypto session is independent of the path: switching paths does
not rekey, and a packet sent over any path uses the same encryption
keys and counter sequence. This is why a relay-tunnelled session can
seamlessly upgrade to direct UDP once hole punching succeeds.

## Epoch

The generation counter for the symmetric encryption keys. Each direction
of a connection has its own key per epoch. Epochs advance via in-place
**rekey**:

- A new ephemeral key exchange happens inside the encrypted session.
- Both sides derive new keys for the next epoch.
- Old-epoch keys are retained briefly so in-flight packets still
  decrypt, then wiped once the rekey is acknowledged.

The epoch field is two bits on the wire (so it wraps at 4) — the
protocol distinguishes "old" vs "new" by direction of change, not by
absolute number. Rekey gives forward secrecy past key compromise:
breaking the keys of epoch *N* does not yield the keys of *N+1*.

## How these compose

A typical session looks like this conceptually:

```
Application
   │
   │  open_stream() / open_channel() / send() / recv()
   ▼
Connection ────────── multiplexes ────────► Streams, Channels
   │                                          (independent flow control,
   │                                           independent loss recovery)
   ▼
  Path  ────► Direct UDP   ─┐
        ◄──── Tunnel relay ─┘   (interchangeable; can swap mid-session)
   │
   ▼
Identity / PeerId ────► verified once during handshake;
                        every packet thereafter is AEAD-protected
                        with epoch-scoped keys.
```

Hold this picture in mind while reading [transport.md](transport.md)
or the crate-level docs.
