# ntied

A decentralized peer-to-peer messenger with end-to-end encryption and
voice calls, written in Rust.

## Project status

Under active development and **not intended for production use**. This
is an experimental implementation created to explore low-level aspects
of network protocols and post-quantum cryptography.

## What it does

- **End-to-end encrypted** sessions over UDP, with hybrid classical +
  post-quantum cryptography (X25519 + ML-KEM-768 for key exchange,
  Ed25519 + ML-DSA-65 for signatures, ChaCha20-Poly1305 for AEAD).
- **Forward secrecy** via in-place key rotation per connection.
- **Decentralized identity** — long-lived keys generated locally,
  identified by a hash-based `PeerId`. No central authority.
- **P2P with relay fallback** — direct UDP between peers when NAT
  permits; multiplexed relay tunnelling with hole-punch upgrade
  otherwise.
- **Multiplexing** — reliable streams (TCP-like) and semi-reliable
  message channels share one connection.
- **Voice calls** and **text messaging** in the desktop app, with an
  encrypted local store (SQLite + sqlcipher, Argon2id KDF).

## Documentation

Conceptual docs live in [`docs/`](docs/):

| Document | What it covers |
|----------|----------------|
| [docs/README.md](docs/README.md) | Index and reading order |
| [docs/architecture.md](docs/architecture.md) | Workspace layout and crate boundaries |
| [docs/concepts.md](docs/concepts.md) | Entities: PeerId, Identity, Node, Connection, Stream, Channel, Relay, Path, Epoch |
| [docs/transport.md](docs/transport.md) | How the transport behaves: handshake, multiplexing, multi-path, key rotation |
| [docs/security.md](docs/security.md) | Cryptographic model, threat model, non-goals |


## Workspace layout

| Crate | Role |
|-------|------|
| [`ntied-transport`](ntied-transport/) | Encrypted UDP transport — protocol, crypto, streams, channels, relay, multi-path |
| [`ntied-server`](ntied-server/) | Standalone relay binary built on `ntied-transport` |
| [`ntied`](ntied/) | Desktop application — UI, chat, calls, local storage |

`ntied` and `ntied-server` both depend on `ntied-transport`. There is
no separate crypto crate.

## Building

Requirements:
- Rust 1.85+ (Cargo workspace, edition 2024).
- Native dependencies for SQLite/sqlcipher are bundled.

```bash
git clone https://github.com/udovin/ntied.git
cd ntied
cargo build --release
```

Run the desktop app:

```bash
cargo run --release --bin ntied
```

Multiple profiles on one machine (each with its own data directory):

```bash
NTIED_PROFILE_DIR=/tmp/ntied-alice cargo run --release --bin ntied
NTIED_PROFILE_DIR=/tmp/ntied-bob   cargo run --release --bin ntied
```

Run a relay server (binds `0.0.0.0:39045` by default):

```bash
cargo run --release --bin ntied-server -- 0.0.0.0:39045
```

### Nix

A flake is provided for reproducible builds and a development shell:

```bash
# Build the default (host-native) binaries
nix build

# Enter the development shell with the Rust toolchain and dependencies
nix develop

# Cross-compile the workspace for Windows (x86_64-pc-windows-gnu)
nix build .#packages.x86_64-linux.ntied-windows
```

## Testing

```bash
# Run all tests
cargo test --workspace

# Run tests for a specific module
cargo test -p ntied-transport
```

## Roadmap

- [ ] Discovery (DHT or similar) — currently relay addresses are configured
- [ ] Congestion control in the transport
- [ ] Periodic rekey timer
- [ ] Group chats
- [ ] File transfers
- [ ] Screen sharing and video calls (foundations in place)
- [ ] Mobile applications
- [ ] Offline message delivery

## License

Apache License 2.0. See [LICENSE.txt](LICENSE.txt).

## Security

This project has not undergone a professional security audit. Do not
use it for transmitting critical information. See
[docs/security.md](docs/security.md) for the threat model and known
non-goals.

Please report security vulnerabilities privately rather than as public
GitHub issues.

## Contributing

Bug reports, feature suggestions, and pull requests are welcome.

By submitting a contribution you agree that, unless you explicitly
state otherwise, any contribution intentionally submitted for inclusion
in the work shall be licensed under the terms of the Apache License,
Version 2.0 (see `LICENSE.txt` and Apache-2.0 §5). You certify that
your contribution is your original work, or that you have the right to
submit it under these terms.

## Acknowledgments

This project is inspired by existing decentralized messengers (Tox,
Jami) and was created to explore modern approaches to P2P
communication and cryptography.
