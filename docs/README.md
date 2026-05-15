# ntied — documentation

ntied is a peer-to-peer messenger with end-to-end encryption, organised
as a small workspace: a transport library that does the heavy lifting
(crypto, reliability, NAT traversal), a thin relay binary, and a desktop
application on top.

This directory is the **conceptual** documentation for the project. It
explains what each part is, what the entities mean, and how they fit
together. Wire formats, frame layouts, and state-machine details live
inside the crate they describe.

## Contents

| Document | What it covers |
|----------|----------------|
| [architecture.md](architecture.md) | Workspace layout, crate boundaries, who depends on whom |
| [concepts.md](concepts.md) | The vocabulary: PeerId, Identity, Node, Connection, Stream, Channel, Relay, Path, Epoch |
| [transport.md](transport.md) | How `ntied-transport` behaves: handshake, multiplexing, multi-path, relay, key rotation |
| [security.md](security.md) | Cryptographic model, forward secrecy, threat model, non-goals |
| [notes.md](notes.md) | Known limitations and sketches of future work |

## Where the implementation details live

There are no separate per-crate doc directories. Wire formats, frame
layouts, packet-byte offsets, module maps, and internal state names
live next to the code they describe — `rustdoc` comments on the
public API and ordinary comments on private items.

If you find yourself wanting to repeat a byte offset or a state name
here, **don't**. Either link to the source, or rephrase it as a
concept that survives a refactor.

## Reading order

- New to the project: [architecture.md](architecture.md) → [concepts.md](concepts.md).
- Integrating against the transport: [transport.md](transport.md), then the source under `ntied-transport/src/`.
- Reviewing the crypto: [security.md](security.md), then `ntied-transport/src/crypto/` for algorithm specifics.
- Wondering why something is missing: [notes.md](notes.md).
