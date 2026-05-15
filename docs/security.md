# Security model

This page describes what the network layer is trying to defend against,
what cryptographic tools it uses, and what it explicitly does **not**
attempt. Algorithm-level specifics (parameter sizes, key derivation
inputs, nonce construction) live in `rustdoc` next to the
implementation under `ntied-transport/src/crypto/` and
`ntied-transport/src/wire/`; this page is the conceptual companion.

> **Status.** The project is experimental and unaudited. Do not use it
> for traffic where compromise would have real-world consequences.

## What a peer must know to reach you

A peer needs two things:

- An **address it can send UDP to** — either your own (direct) or a
  relay where you are reachable.
- Your **PeerId**.

PeerId is the hash of your long-term public key. There is no central
identity service: identities are generated locally and proven during
the handshake.

## Goals

The transport aims to provide, between two peers that complete a
handshake:

- **Mutual authentication.** Each side proves possession of the
  identity it claims, signed against a transcript that captures the
  key exchange.
- **Confidentiality.** Application payloads — stream bytes, channel
  messages, control frames — are encrypted in transit. The relay
  sees PeerIds and ciphertext only.
- **Integrity.** Tampering with any encrypted packet (including its
  header) is detected by AEAD verification.
- **Forward secrecy.** Compromising the long-term identity key does
  not retroactively expose past sessions; compromising one epoch's
  keys does not expose other epochs.
- **Post-quantum hedging.** A future adversary with a quantum
  computer should not be able to break a recorded session by
  attacking the classical half alone.

## Cryptographic shape (conceptually)

ntied uses **hybrid** constructions throughout: a classical primitive
and a post-quantum primitive run side by side, and the protocol fails
unless both succeed.

| Role | Classical | Post-quantum |
|------|-----------|--------------|
| Key exchange (KEM) | X25519 | ML-KEM-768 |
| Signatures | Ed25519 | ML-DSA-65 |
| AEAD | ChaCha20-Poly1305 | — (symmetric algorithms are already PQ-safe) |
| Hash | SHA3-256 | — |

Hybrid means:

- The session key is derived from the concatenation of both KEM secrets,
  so an attacker must break **both** X25519 and ML-KEM-768 to recover
  it.
- A signature verifies only if **both** the Ed25519 component and the
  ML-DSA-65 component verify. Forging requires breaking both schemes.

This hedges against either family being broken in the future without
betting the protocol on the security of just one.

### Key separation by direction and epoch

Each connection derives **separate keys per direction**
(initiator→responder, responder→initiator) and **per epoch**. Rekey
advances the epoch in place, derives a fresh pair of keys, and wipes
the previous ones once they are no longer needed for in-flight
packets. Forward secrecy is direct: keys of epoch *N* are not
recoverable from keys of any other epoch.

### Replay and freshness

Each Data packet carries a monotonically increasing counter. The
counter feeds into the AEAD nonce, into duplicate detection, and into
ACK bookkeeping. A receiver tracks the highest counter it has seen and
a sliding window of recent counters, so out-of-order delivery is fine
but replays are rejected.

## What an attacker on the wire sees

For traffic on a direct path, an on-path attacker observes:

- UDP packet sizes and timing.
- The unencrypted packet header: type byte, connection IDs, packet
  counter, epoch.
- The opaque ciphertext, including a 16-byte AEAD tag.

They cannot read frame contents, learn stream/channel IDs, learn the
peers' identities (the handshake transports public keys *inside* the
encrypted post-handshake exchange — only the KEM public key and
ciphertext are sent in cleartext during the very first round trip),
or replay any packet without detection.

For traffic on a relay path, the relay sees the above **plus** the
destination PeerId in the tunnel-message header. It does not see the
identity of the connection's peers from the relay's own session keys'
perspective on the *inner* packet, because the inner packet is
encrypted under end-to-end keys that the relay never negotiates with
either peer.

## What is *not* defended against

The transport is not trying to solve, and cannot solve:

- **Traffic analysis.** Packet sizes, timing, and the fact that two
  peers are talking through a known relay are all visible. There is
  no padding, no cover traffic, no onion routing.
- **Endpoint compromise.** If an attacker controls one of the peer
  machines, all bets are off — the application sees plaintext.
- **Denial of service.** An attacker who can flood the UDP socket can
  prevent legitimate traffic. Resource limits keep memory bounded
  (bounded stream/channel buffers, capped open IDs), but there is no
  bandwidth-level mitigation.
- **Censorship resistance.** The protocol uses a recognisable wire
  format with a fixed first byte per packet type; a censor with
  per-packet inspection can identify and drop ntied traffic.
- **Metadata at the relay.** A relay observes which PeerIds talk to
  which PeerIds, and when. Run your own relay or use a direct path
  to avoid this.
- **Long-term anonymity.** PeerId is stable for the life of an
  identity. Anonymity from a passive observer requires rotating
  identities, which is an application concern.

## Identity lifecycle

`PrivateKey::generate()` produces a fresh identity from the OS RNG.
`to_bytes` / `from_bytes` round-trips the full keypair, intended for
local persistence — ideally inside an encrypted store (the desktop app
uses `sqlcipher` with an Argon2id-derived key).

Losing the private key means losing the identity: there is no recovery
mechanism, because there is no authority that could re-issue it.
Sharing the private key clones the identity onto another machine; any
of those machines can then authenticate as that PeerId.

## Reporting issues

Security issues should be reported privately rather than as public
GitHub issues. Treat any finding affecting authentication, key
derivation, or AEAD verification as load-bearing — a bug there is a
bug in the protocol, not in a feature.
