# ntied

A decentralized peer-to-peer messenger with end-to-end encryption and voice calls, written in Rust.

## Project Status

This project is under active development and **is not intended for production use**. This is an experimental implementation created to explore low-level aspects of network protocols and cryptography.

## Description

ntied is a decentralized messenger designed for secure communication without dependence on centralized servers. The project focuses on privacy, security, and stable operation of basic features.

### Key Features

- **End-to-end encryption** — all messages and calls are protected by elliptic curve cryptography (p256, ECDSA, AES-GCM)
- **Perfect Forward Secrecy** — ephemeral key rotation system protects message history even if long-term keys are compromised
- **P2P architecture** — direct connection between peers over UDP with automatic NAT traversal
- **Text messaging** — basic messaging with local storage in an encrypted database
- **Voice calls** — real-time audio communication with minimal latency
- **Local storage** — SQLite with encryption via sqlcipher and Argon2id password hashing

## Architecture

The project is divided into several interconnected modules:

### Modules

- **ntied-crypto** — cryptographic module for working with keys, digital signatures, and encryption
- **ntied-transport** — low-level transport protocol over UDP with NAT traversal support
- **ntied-server** — auxiliary server for address exchange when establishing P2P connections
- **ntied** — main application with business logic, UI, and audio

### Technologies

- **Rust** (edition 2024) — primary development language
- **Tokio** — asynchronous runtime
- **iced** — cross-platform GUI framework
- **cpal** — audio device handling
- **SQLite + sqlcipher** — encrypted data storage
- **serde + bincode** — protocol serialization

## How It Works

### Transport Protocol

The protocol operates over UDP and provides:
- Secure connection establishment via Handshake with key exchange (ECDH)
- Encryption of all packets after connection establishment (AES-GCM)
- Heartbeat for connection liveness monitoring
- Epoch system with key rotation for Perfect Forward Secrecy

### NAT Traversal

A minimalist server is used to overcome NAT:
1. Clients register on the server by sending their public key
2. When initiating a connection, the server exchanges addresses of both peers
3. Clients simultaneously send UDP packets to each other (UDP hole punching)
4. Direct P2P connection is established

### Security

- Long-term keys on elliptic curves (p256)
- Ephemeral keys for each epoch with automatic rotation
- ECDSA digital signatures for authentication
- AES-GCM for traffic encryption
- Argon2id for local storage password hashing

## Building and Running

### Requirements

- Rust 1.82+ (with edition 2024 support)
- OpenSSL (for sqlcipher)

### Building

```bash
# Clone the repository
git clone https://github.com/yourusername/ntied.git
cd ntied

# Build all modules
cargo build --release

# Run the application
cargo run --release
```

### Running the NAT traversal server

```bash
cd ntied-server
cargo run --release -- --host 0.0.0.0 --port 8080
```

## Testing

```bash
# Run all tests
cargo test --workspace

# Run tests for a specific module
cargo test -p ntied-transport
```

## Project Structure

```
ntied/
├── ntied/              # Main application
│   ├── src/           # Application source code
│   ├── assets/        # Resources (icons, images)
│   └── tests/         # Integration tests
├── ntied-crypto/      # Cryptographic module
├── ntied-transport/   # Transport protocol
├── ntied-server/      # NAT traversal server
└── scripts/           # Utility scripts
```

## Roadmap

- [ ] Group chats
- [ ] File transfers
- [ ] Screen sharing
- [ ] Video calls
- [ ] Mobile applications
- [ ] Enhanced NAT traversal (STUN/TURN)
- [ ] Offline message support

## License

**ntied is free for personal use!**

This project is licensed under the **Apache License 2.0 with Commons Clause**, which means:

### You CAN:
- Use it freely for personal communication with friends and family
- Study the code and learn from it
- Modify and customize it for your own needs
- Share it with others for free
- Contribute improvements back to the project
- Use it for educational and research purposes

### You CANNOT:
- Sell the software or offer it as a paid service
- Use it in commercial products without permission
- Charge for hosting or support services primarily based on this software

### Commercial Use
If you need to use ntied commercially, please contact us to discuss licensing options. We're open to reasonable arrangements once the project matures.

See [LICENSE.md](LICENSE.md) for complete licensing details.

## Security

**Important**: This project has not undergone professional security audit. Do not use it for transmitting critical information.

If you discover a security vulnerability, please report it privately rather than creating a public issue.

## Contributing

We welcome contributions to ntied! The project is in early development, and we appreciate bug reports, feature suggestions, and code contributions.

### Contributor Rights

By submitting a contribution to this project, you agree that:

1. **You grant the project maintainers a perpetual, worldwide, non-exclusive, royalty-free license** to use, modify, and relicense your contribution under any license terms, including the ability to relicense under different terms in the future (such as moving from Apache 2.0 + Commons Clause to pure Apache 2.0).

2. **You retain copyright** to your contributions, but grant us the flexibility to change the project's license if needed.

3. **Your contribution is your original work** or you have the right to submit it under these terms.

This flexible licensing arrangement allows us to:
- Potentially move to a more permissive license (like pure Apache 2.0) in the future
- Offer commercial licenses without legal complications
- Ensure the project can evolve as the ecosystem changes

### How to Contribute

- Report bugs and issues on GitHub
- Suggest new features or improvements
- Submit pull requests with code improvements
- Improve documentation
- Help with testing and feedback

Detailed contribution guidelines will be established as the project matures.

## Acknowledgments

This project is inspired by existing decentralized messengers (Tox, Jami) and was created to explore modern approaches to P2P communication and cryptography.
