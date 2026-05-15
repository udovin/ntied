# Architecture

ntied is a Cargo workspace. Three crates, one direction of dependency.

```
┌────────────────────────────────────────────────────────────┐
│  ntied                                                     │
│  Desktop application — UI, chat, calls, local storage      │
│  iced + cpal + sqlcipher                                   │
└────────────────────────┬───────────────────────────────────┘
                         │ uses
                         ▼
┌────────────────────────────────────────────────────────────┐
│  ntied-transport                                           │
│  Encrypted UDP transport — handshake, streams, channels,   │
│  multi-path, relay tunneling, key rotation                 │
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
channels, multi-path with relay fallback, key rotation. Exposes
two layers:

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
│  ntied   │◄────────►│  relay   │◄────────►│  ntied   │
│  (peer)  │           │ (server) │           │  (peer)  │
└──────────┘           └──────────┘           └──────────┘
      ▲                                            ▲
      └────────────── direct UDP ───────────────────┘
        (after successful hole punch)
```

Two clients meet at a known relay address, register, and exchange
traffic through the relay. In parallel, they attempt UDP hole punching
via the relay's control channel; if it succeeds, traffic migrates onto
a direct path and the relay path is held as a fallback.

## What is *not* in the workspace today

- No DHT-based discovery. Relay addresses are configured.
- No multi-hop relay routing (the relay forwards between its own
  registered clients only).
- No mobile crate.

Future direction for those gaps is in [notes.md](notes.md). Treat that
document as a sketch of intent, not as something the code already
implements.
