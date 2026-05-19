# Architecture

ntied is a Cargo workspace. Three crates, one direction of dependency.

```
┌────────────────────────────────────────────────────────────┐
│  ntied                                                     │
│  Desktop application — UI, chat, calls, local storage      │
│  iced + cpal + sqlcipher                                   │
└────────────────────────┬───────────────────────────────────┘
                         │ uses
┌────────────────────────▼───────────────────────────────────┐
│  ntied-transport                                           │
│  Encrypted UDP transport — handshake, streams, channels,   │
│  multi-path, relay tunneling, key rotation, DHT discovery  │
└────────────────────────▲───────────────────────────────────┘
                         │ uses
┌────────────────────────┴───────────────────────────────────┐
│  ntied-server                                              │
│  Standalone relay binary — runs `Node::serve_as_relay()`   │
└────────────────────────────────────────────────────────────┘
```

`ntied-server` is a thin wrapper: it binds a `Node` and runs it in
relay-server mode. All the protocol logic lives in `ntied-transport`.

## Crate responsibilities

### ntied-transport

Everything network-facing: identity, post-quantum hybrid handshake,
encrypted UDP transport, reliable streams, semi-reliable message
channels, multi-path with relay fallback, key rotation, and a
BitTorrent-DHT–based discovery layer (peer + relay records).
Exposes two layers:

- A synchronous, I/O-free state machine (`connection::Connection`) —
  testable without sockets.
- An async wrapper (`node::Node`) — Tokio event loop, UDP socket,
  per-peer connections, relay multiplex.

Application code typically only sees the async layer.

See [transport.md](transport.md) for behaviour; module-level and
wire-format detail lives in `rustdoc` next to the code in
`ntied-transport/src/`.

### ntied-server

A binary that runs a `Node` as a public relay. Its job is to:

- Accept connections from clients behind NAT.
- Forward multiplexed traffic between them (tunnel channel).
- Carry hole-punch signalling (control channel) so two clients can
  upgrade to a direct path.

The relay never sees plaintext peer payloads — only the destination
PeerId in each tunnel message header.

### ntied

The desktop app. It owns user-visible concerns: contacts, message
history, call setup, audio capture/playback, GUI. It connects to
`ntied-transport`'s async API and treats the network as a set of
peer connections with streams and channels.

This crate is out of scope for the rest of the docs in this folder —
the conceptual docs focus on the network layer.

## Dependency direction

- `ntied` → `ntied-transport`
- `ntied-server` → `ntied-transport`
- `ntied-transport` depends only on third-party crates (no internal deps)

There is no `ntied-crypto` crate: crypto lives inside `ntied-transport`
under `src/crypto/`.

## Process layout in a typical deployment

```
┌──────────┐   UDP     ┌──────────┐   UDP     ┌──────────┐
│  ntied   │◄─────────►│  relay   │◄─────────►│  ntied   │
│  (peer)  │           │ (server) │           │  (peer)  │
└──────────┘           └──────────┘           └──────────┘
      ▲                                            ▲
      └────────────── direct UDP ──────────────────┘
        (after successful hole punch)
```

Two clients meet at a relay (either pre-configured via
`Node::attach_relay(addr)` or discovered via the DHT — see
[Discovery layer](#discovery-layer) below), register, and exchange
traffic through it. In parallel, they attempt UDP hole punching
via the relay's control channel; if it succeeds, traffic migrates onto
a direct path and the relay path is held as a fallback.

## Discovery layer

Peers can also be reached without a pre-configured relay address.
`ntied-transport` runs a BitTorrent mainline DHT actor (the `mainline`
crate) per Node and uses BEP-5 announce/lookup to publish three
info-hashes:

- `H_peer_direct(peer_id)` — the peer's own white-IP UDP socket
  (announced by the peer itself via `enable_public_peer`).
- `H_peer_relay(peer_id)` — relay addresses that route to this peer
  (announced by each relay when it accepts the peer).
- `H_relays` — open registry of public relays (relay opt-in via
  `enable_public_relay` and the `ntied-server --publish-dht` flag).

`Node::connect_peer(peer_id)` is the DHT-driven outbound: it looks up
the routes and tries direct first, via-relay as fallback.  The DHT
actor owns a separate UDP socket from the transport socket.  See
[transport.md](transport.md) and the source under
`ntied-transport/src/discovery.rs`.

## What is *not* in the workspace today

- No multi-hop relay routing (the relay forwards between its own
  registered clients only).
- No mobile crate.

Future direction for those gaps is in [notes.md](notes.md). Treat that
document as a sketch of intent, not as something the code already
implements.
