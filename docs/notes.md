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
- **No periodic rekey trigger.** The rekey state machine works
  end-to-end, but nothing fires it on a schedule. Long-lived
  connections currently reuse the same epoch keys indefinitely.
- **Timeout-based loss recovery is incomplete.** The
  `loss_detection_pending` flag is raised on timeout but the
  retransmit path does not act on it in every case. A reproducer
  test is in the crate, named `bug_timeout_loss_detection_never_retransmits`.
- **Auth-fragment loss can hang the handshake.** Auth fragments go
  through the message fragmenter but are not tracked in send-side
  ACK state, so a lost auth fragment is not retransmitted until
  the handshake timeout fires. Reproducer:
  `bug_auth_frame_loss_hangs_handshake`.

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

### Discovery (DHT)

The transport assumes peer addresses (or relay addresses) are known
out of band. A Kademlia-style DHT layered on top of the existing
encrypted gateway-to-gateway connections is the planned mechanism:
each "gateway peer" doubles as a DHT node, clients publish their
reachability record via their gateway, and resolution is an iterative
XOR lookup. PeerIds already have the right shape (32-byte hash) to
serve as DHT keys.

### Multi-hop relay

The current relay forwards only between its own registered clients.
A `GatewayForward` frame between gateways, plus a small reverse-route
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

### Private-server / restricted routing

A gateway becomes a "private server" by adding an authorisation hook
to registration and a routing policy on the DHT record that restricts
which gateways may route to a given peer. The frame format already
reserves room for an `auth_data` field; the policy is a future field
on the discovery record.

### Path negotiation between peers

Two peers might want to migrate onto a private relay only they trust.
A pair of E2E frames (`PathSuggest` / `PathSuggestAck`) — visible only
to the two endpoints, not to any gateway — is the planned signalling.
