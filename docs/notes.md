# Notes

A running list of things the code does *not* yet do and where it is
heading. Useful for new contributors who need to know what to avoid
relying on and what is on deck.

## Known limitations

These are intentional or accepted gaps in the current transport. They
do not block normal use but they should land before the project is
considered production-grade.

### Must fix

- **No congestion control.** A fast sender can saturate the network
  and trigger loss spirals. A CUBIC- or BBR-like algorithm with
  pacing is the planned shape.
- **Timeout-based loss recovery is incomplete.** The
  `loss_detection_pending` flag is raised on timeout but the
  retransmit path does not act on it in every case. A reproducer
  test is in the crate, named `bug_timeout_loss_detection_never_retransmits`.
- **Auth-fragment loss can hang the handshake.** Auth fragments go
  through the message fragmenter but are not tracked in send-side
  ACK state, so a lost auth fragment is not retransmitted until
  the handshake timeout fires. Reproducer:
  `bug_auth_frame_loss_hangs_handshake`.
- **Handshake transcript is thin.** The transcript hash currently
  covers only the hybrid KEM public key and the KEM ciphertext. It
  does not bind a protocol/version label, role tags, connection IDs,
  or the expected `PeerId`. Practical impact is limited (per-direction
  keys and AEAD-protected headers carry the load), but it is thinner
  than a formally reviewable AKE wants.  Tracked as a wire-format
  change for a future revision.

### Nice to have

- **Connection error codes.** Today the close error code is ad-hoc
  (`0` graceful, `1` too many streams, `2` too many channels).
  Should become a typed enum.
- **Stream priority.** Round-robin scheduling only; no way to
  prioritise one stream over another.
- **Idle keepalive at the connection level.** Currently delegated to
  the `Node` wrapper.
- **Connection migration.** Changing peer address mid-session without
  re-handshaking. The path abstraction already supports this; the
  signalling does not.
- **0-RTT data.** Sending application data in the `Init` packet using
  cached peer keys.

## Future work

Sketched designs from earlier planning passes. None of this is
implemented; it is recorded so the existing abstractions stay friendly
to it.

### Multi-hop relay

The current relay forwards only between its own registered clients.
A `RelayForward` frame between relays, plus a small reverse-route
cache, would let traffic chain across multiple relays. Encryption
overhead does not grow with hop count because the inner E2E packet is
opaque to every intermediary.

### Multi-path with smart failover

Path abstraction already exists per connection
(see [concepts.md](concepts.md#path)). What is missing is:

- automatic failover when an active path goes quiet for a few RTTs,
- promotion from relay to direct as soon as a hole-punched path
  validates,
- promotion to a shorter relay path when one becomes available.

The session's encryption state is path-independent (same keys, same
counter sequence), so switching paths mid-session is already supported
at the protocol level.

### Private relays / restricted routing

Today every relay accepts every client.  A future opt-in could let a
relay run in "private" mode, where it serves only an allow-listed set
of `PeerId`s (and refuses to publish them in `H_peer_relay`).  This
would also need an authorisation hook on the `attach_relay` side so
clients prove they belong to that list.

### Path negotiation between peers

Two peers might want to migrate onto a private relay only they trust.
A pair of E2E frames (`PathSuggest` / `PathSuggestAck`) — visible only
to the two endpoints, not to any relay — is the planned signalling.
