# ntied-transport — Gateway, Relay & DHT Plan

> **Progress (last session):** Phases 1–6 implemented. 230 unit/codec tests pass.
> Relay integration test (`relay_through_gateway`) has a bug — see notes in Phase 6 below.

> **MTU policy**: `INITIAL_MTU = 1350` (up from 1200). Safe for virtually all
> networks (Ethernet 1500 − IP 20 − UDP 8 = 1472; leaves room for VPN/tunnel
> overhead). This avoids relay-level fragmentation entirely.

## 1. Architecture Decisions

| # | Decision | Rationale |
|---|----------|-----------|
| 1 | Crypto, sessions, streams, ACK — without changes | Working, tested, no reason to touch |
| 2 | Gateway = regular peer with special frame handling | No new packet types, reuses existing Data encryption |
| 3 | Relay = GatewayRelay/Deliver frames inside Data packets | E2E blob is opaque, overhead doesn't grow with hops |
| 4 | DHT = frames inside Data packets between gateways | Free encryption and authentication |
| 5 | Keep HolePunch packet (0x03) for raw UDP probes | Needed for actual NAT traversal, no session exists yet |
| 6 | Remove Relay packet (0x04) | Unused, relay is now via frames |
| 7 | Keep rekey for both gateway sessions and E2E sessions | Long-lived sessions need key rotation |
| 8 | Session ≠ Path | E2E crypto state is independent of transport path |
| 9 | Private server = gateway with auth + routing policy | Architecture now, implement later |

---

## 2. Current State

### What works

- Direct connection: handshake, encryption, reliable streams, datagram streams
- NAT hole punching: HolePunch packet burst, auto-cancel on response
- Discovery: trait-based (`HashMapDiscovery` for tests, `ServerDiscovery` for centralized server)
- Keepalive, graceful close, rekey state machine

### What's missing

- Relay (defined but not implemented)
- DHT discovery
- Gateway concept
- Multi-path / failover
- Path negotiation between peers

---

## 3. Relay Model

### How it works

```
A (behind NAT)          GW                B (behind NAT)
    │                    │                    │
    │  Connection A↔GW   │  Connection GW↔B   │
    │  (ntied session)   │  (ntied session)   │
    │                    │                    │
    │  GatewayRelay      │  GatewayDeliver    │
    │  {dest=B,          │  {src=A,           │
    │   inner=blob}      │   inner=blob}      │
    │ ──────────────────>│ ──────────────────>│
    │                    │                    │
    │  GatewayDeliver    │  GatewayRelay      │
    │  {src=B,           │  {dest=A,          │
    │   inner=blob}      │   inner=blob}      │
    │ <──────────────────│ <──────────────────│
```

`blob` is an E2E encrypted packet (Data packet with session A↔B keys).
Gateway sees only `(src_peer_id, dest_peer_id, opaque blob)`.
Blob is identical at every hop.

### Multi-gateway relay

```
A → GW1: Data(session A↔GW1) { GatewayRelay { dest=B, inner=BLOB } }
GW1 → GW2: Data(session GW1↔GW2) { GatewayForward { dest=B, src=A, inner=BLOB } }
GW2 → B: Data(session GW2↔B) { GatewayDeliver { src=A, inner=BLOB } }
```

### Encryption layers

| Segment | Encryption | What the intermediary sees |
|---------|------------|---------------------------|
| Client ↔ GW | Session Client↔GW | GW sees frame: dest/src PeerId + opaque inner |
| GW ↔ GW | Session GW↔GW | GW sees frame: dest/src PeerId + opaque inner |
| E2E | Session A↔B | Only A and B see payload |

### MTU budget

```
Initial MTU:                            1350 bytes

Data packet overhead:                     33 bytes
  packet_type + epoch:  1
  session_id:           8
  counter:              8
  poly1305_tag:        16

GatewayRelay frame overhead:             36 bytes
  frame_type:           1
  frame_length:         2
  dest_peer_id:        33

Available for E2E inner packet:         1281 bytes
E2E Data packet overhead:                33 bytes
Available for E2E frames:               1248 bytes

KeyExchangeInit (1258 bytes):           fits ✅  (1281 − 1258 = 23 bytes margin)
KeyExchangeResponse (1137 bytes):       fits ✅  (1281 − 1137 = 144 bytes margin)
```

No relay-level fragmentation needed. Overhead does not depend on the number of hops.

---

## 4. Changes to Existing Code

### 4.1 PeerConnection — bubble up unhandled frames

Current `on_data_packet` processes all frames internally.
Gateway/DHT/path frames must be returned to the caller.

```rust
// Before:
pub fn on_data_packet(&mut self, data: Data, now: Instant)

// After:
pub fn on_data_packet(&mut self, data: Data, now: Instant) -> Vec<Frame>
```

PeerConnection handles: Ack, Ping, Pong, Stream*, WindowUpdate, DatagramFragment,
Datagram, Auth, AuthComplete, Rekey, RekeyAck, ConnectionClose.
Everything else (0x10+) is returned as unhandled.

### 4.2 ConnEntry — transport path abstraction

```rust
// Before:
struct ConnEntry {
    peer_addr: SocketAddr,
    conn: Box<PeerConnection>,
    ...
}

// After:
enum TransportPath {
    Direct { addr: SocketAddr },
    Relayed { gateway_session_id: u64, dest_peer_id: PeerId },
}

struct ConnEntry {
    path: TransportPath,
    conn: Box<PeerConnection>,
    ...
}
```

Sending:
- `Direct` → `socket.send_to(data.encode(), addr)`
- `Relayed` → wrap in GatewayRelay frame, send through gateway connection

### 4.3 Discovery — richer return type

```rust
// Before:
async fn resolve(&self, peer_id: &PeerId) -> Option<SocketAddr>;

// After:
pub enum RouteInfo {
    Direct(SocketAddr),
    Relayed { gateway_addr: SocketAddr },
}

async fn resolve(&self, peer_id: &PeerId) -> Option<RouteInfo>;
```

`HashMapDiscovery` returns `RouteInfo::Direct` — backward compatible for tests.

### 4.4 Public API — Transport → Node

```rust
pub struct Node { ... }

impl Node {
    /// Bind to a UDP socket
    pub async fn bind(addr: SocketAddr, identity: PrivateKey) -> io::Result<Self>;

    /// Direct connection by address (like TCP — specify addr, connect)
    pub async fn connect_addr(&self, addr: SocketAddr) -> io::Result<Connection>;

    /// Connect by PeerId (discovery → direct or relay)
    pub async fn connect(&self, peer_id: &PeerId) -> io::Result<Connection>;

    /// Accept incoming connections (direct or relayed)
    pub async fn accept(&self) -> io::Result<Connection>;

    /// Join the network through gateway(s)
    pub async fn join_network(&self, config: NetworkConfig) -> io::Result<()>;

    pub fn local_addr(&self) -> io::Result<SocketAddr>;
    pub fn peer_id(&self) -> PeerId;
}

pub struct NetworkConfig {
    pub bootstrap: Vec<SocketAddr>,
    pub preferred_gateway: Option<PeerId>,
}

impl Connection {
    pub fn transport_path(&self) -> &TransportPath;
    // everything else unchanged
}
```

---

## 5. New Modules

```
src/
├── gateway/
│   ├── mod.rs
│   ├── client.rs      — register on GW, send/receive relay, hole punch request
│   ├── server.rs      — accept registrations, route relay traffic
│   └── routing.rs     — route table, reverse route cache
│
├── dht/
│   ├── mod.rs
│   ├── kademlia.rs    — k-buckets, XOR distance, node management
│   ├── record.rs      — DhtRecord, signature, verification
│   └── protocol.rs    — FindNode, Publish, Query, Store handlers
```

---

## 6. New Frame Types

### Gateway Control (0x10–0x16)

| Type | Name | Direction | Payload |
|------|------|-----------|---------|
| 0x10 | GatewayRegister | Client → GW | `peer_id: PeerId, flags: u16, auth_data_len: u16, auth_data: [u8]` |
| 0x11 | GatewayRegisterAck | GW → Client | `status: u8, relay_mtu: u16` |
| 0x12 | GatewayRelay | Client → GW | `dest_peer_id: PeerId, inner_len: u16, inner: [u8]` |
| 0x13 | GatewayDeliver | GW → Client | `src_peer_id: PeerId, inner_len: u16, inner: [u8]` |
| 0x14 | HolePunchRequest | Client → GW | `target_peer_id: PeerId` |
| 0x15 | HolePunchNotify | GW → Client | `requester_peer_id: PeerId, addr_count: u8, addrs: [SocketAddr]` |
| 0x16 | GatewayForward | GW → GW | `dest_peer_id: PeerId, src_peer_id: PeerId, ttl: u8, inner_len: u16, inner: [u8]` |

### DHT (0x20–0x25)

| Type | Name | Direction | Payload |
|------|------|-----------|---------|
| 0x20 | DhtFindNode | GW ↔ GW | `target: PeerId, request_id: u32` |
| 0x21 | DhtFindNodeReply | GW ↔ GW | `request_id: u32, node_count: u8, nodes: [DhtNode]` |
| 0x22 | DhtPublish | Client → GW | `record: DhtRecord` |
| 0x23 | DhtQuery | Any → GW | `target: PeerId, request_id: u32` |
| 0x24 | DhtQueryReply | GW → Any | `request_id: u32, status: u8, record: DhtRecord` |
| 0x25 | DhtStore | GW → GW | `record: DhtRecord` |

### Path Negotiation (0x30–0x31)

| Type | Name | Direction | Payload |
|------|------|-----------|---------|
| 0x30 | PathSuggest | Peer ↔ Peer (E2E) | `gateway_peer_id: PeerId, addr_count: u8, gateway_addrs: [SocketAddr], auth_token_len: u16, auth_token: [u8]` |
| 0x31 | PathSuggestAck | Peer ↔ Peer (E2E) | `gateway_peer_id: PeerId, status: u8` |

### Serialization Types

```rust
struct DhtNode {
    peer_id: PeerId,          // 33 bytes
    addr_count: u8,
    addrs: Vec<SocketAddr>,   // 1 + 4/16 + 2 bytes each
}

struct DhtRecord {
    peer_id: PeerId,
    public_key: PublicKey,
    gateway_count: u8,
    gateways: Vec<GatewayInfo>,
    routing_policy: u8,       // 0x00 = Open, 0x01 = GatewayRestricted
    restricted_gw_count: u8,  // only if routing_policy == 0x01
    restricted_gws: Vec<PeerId>,
    version: u64,
    expires_at: u64,
    signature: Signature,
}

struct GatewayInfo {
    gateway_peer_id: PeerId,
    addr_count: u8,
    addrs: Vec<SocketAddr>,
    latency_hint: u16,
}
```

---

## 7. DHT Design

### Participants

Only gateways are full DHT nodes.
Clients behind NAT interact with DHT through their gateway (DhtPublish, DhtQuery frames).

### Kademlia Parameters

| Parameter | Value |
|-----------|-------|
| Node ID | PeerId of the gateway (32 bytes of hash) |
| Distance | XOR metric |
| K (bucket size) | 20 |
| α (concurrency) | 3 |
| Lookup | O(log n) hops |

### DhtRecord Verification

1. `SHA3-256(record.public_key) == record.peer_id.hash`
2. Verify `signature` over `(peer_id, gateways, routing_policy, version, expires_at)`
3. `record.version > stored_version` (prevents replay)

### Publish Flow

```
Client A            GW-Alpha           GW-X (closest to A in XOR)
   │                   │                    │
   │── DhtPublish ────>│                    │
   │   record={        │                    │
   │     peer_id: A    │── DhtStore ───────>│
   │     gw: [Alpha]   │   (replicated to   │
   │     version: 1    │    K closest nodes) │
   │     sig: ...      │                    │
   │   }               │                    │
```

### Lookup Flow

Standard Kademlia iterative lookup:

1. Select K closest nodes to target from local k-buckets
2. Send DhtFindNode(target) to α closest in parallel
3. Collect closer nodes from replies
4. Repeat until converged
5. Send DhtQuery to K closest → receive DhtRecord

---

## 8. Connection Lifecycle

### Direct (both have public IP)

```
A ──── UDP ────> B

1. A → B: KeyExchangeInit
2. B → A: KeyExchangeResponse
3. Auth fragments exchange (E2E encrypted)
4. Established
```

### Through one gateway

```
A (NAT) ──── GW ──── B (NAT or public)

1. A: resolve(B) → B is on GW
2. A → GW: GatewayRelay { dest=B, inner=KeyExchangeInit }
3. GW → B: GatewayDeliver { src=A, inner=KeyExchangeInit }
4. B → GW: GatewayRelay { dest=A, inner=KeyExchangeResponse }
5. GW → A: GatewayDeliver { src=B, inner=KeyExchangeResponse }
6. Auth via relay (E2E encrypted, same GatewayRelay/Deliver wrapping)
7. Established (over relay)
8. Background: hole punch → direct upgrade if successful
```

### Through two gateways

```
A (NAT) ──── GW1 ──── GW2 ──── B (NAT)

1. A: DHT lookup → B is on GW2
2. A → GW1: GatewayRelay { dest=B, inner=KEInit }
3. GW1 → GW2: GatewayForward { dest=B, src=A, inner=KEInit }
4. GW2 → B: GatewayDeliver { src=A, inner=KEInit }
5. Reverse path for KEResponse
6. Auth via relay (E2E encrypted)
7. Established
8. Background: hole punch via GW signaling → direct upgrade
```

---

## 9. Multi-Path & Failover

### Session ≠ Path

```rust
struct SessionPaths {
    paths: Vec<TransportPath>,
    active: usize,
}

enum TransportPath {
    Direct { addr: SocketAddr, rtt: Duration },
    Relayed { gateway_session_id: u64, dest_peer_id: PeerId, rtt: Duration },
}
```

E2E packet contains nothing tied to the path.
Same encrypted blob can be delivered through any path.

### Path priority

1. Direct (lowest latency)
2. Relayed through shared gateway (1 hop)
3. Relayed through gateway chain (N hops)

### Failover

```
t=0s     Active path (direct) stops responding
t=3s     Ping timeout
t=6s     Retry ping
t=9s     Path marked dead → switch to next best path (relay)

E2E session NOT interrupted:
  - Keys, counters, streams stay in memory
  - Packets flow through new path
  - Counter keeps incrementing → no replay
  - Background: retry hole punch for direct upgrade
```

### Path upgrade via relay optimization

```
Before:  A → GW1 → GW2 → GW3 → B     (3 hops, ~120ms)
Both registered on GW2.
After:   A → GW2 → B                   (1 hop, ~40ms)
```

Same mechanism: add better relay path, switch active.

### Path negotiation between peers

When one peer wants to suggest a better path (e.g. private server):

```
A → B (E2E frame):  PathSuggest { gateway: PeerId(GW-P), addrs: [...], auth_token: ... }
B: connects to GW-P, registers (using auth_token if needed)
B → A (E2E frame):  PathSuggestAck { gateway: PeerId(GW-P), status: OK }
A: both ready → switch active path to GW-P
```

Signaling goes through existing E2E session — encrypted, authenticated.
Gateway cannot see what the peers are negotiating.

---

## 10. Private Server Support

A private server is a gateway with access control.

### Architecture hooks

```rust
// Gateway server authorization
trait GatewayAuth: Send + Sync {
    async fn authorize(&self, peer_id: &PeerId, auth_data: &[u8]) -> bool;
}

// DhtRecord routing policy
enum RoutingPolicy {
    Open,                           // reachable through any gateway
    GatewayRestricted(Vec<PeerId>), // only through these gateways
}
```

### Client configuration

```rust
NetworkConfig {
    bootstrap: vec![private_server_addr],
    preferred_gateway: Some(private_server_peer_id),
}
```

### Flow

1. Client connects to private server (normal ntied handshake)
2. Sends GatewayRegister with auth_token in auth_data
3. Server verifies credentials → GatewayRegisterAck
4. Client publishes DhtRecord with `GatewayRestricted([server_peer_id])`
5. Incoming connections can only reach client through this server
6. For calls: peer sends PathSuggest with server info + auth_token

Implemented later. The frame format and routing policy field support this without protocol changes.

---

## 11. Gateway Server

### Routing logic

```rust
fn handle_relay(&self, frame: GatewayRelay, from_peer_id: PeerId) {
    if let Some(client) = self.local_clients.get(&frame.dest_peer_id) {
        // Client registered here — deliver
        client.send_frame(GatewayDeliver {
            src_peer_id: from_peer_id,
            inner: frame.inner,
        });
    } else if let Some(gw) = self.route_cache.get(&frame.dest_peer_id) {
        // Known to be on another gateway — forward
        gw.send_frame(GatewayForward {
            dest_peer_id: frame.dest_peer_id,
            src_peer_id: from_peer_id,
            inner: frame.inner,
        });
    } else {
        // Unknown — DHT lookup, then forward or drop
        self.lookup_and_forward(frame.dest_peer_id, from_peer_id, frame.inner);
    }
}
```

### Reverse route cache

When a gateway receives GatewayForward from another gateway, it remembers the
reverse route:

```rust
// GW2 receives GatewayForward { dest=B, src=A } from GW1
// GW2 remembers: "A is reachable via GW1"
self.route_cache.insert(src_peer_id, ReverseRoute {
    via_gateway: from_gateway_peer_id,
    last_seen: Instant::now(),
    ttl: Duration::from_secs(120),
});
```

When B responds to A, GW2 already knows the route — no DHT lookup needed.

### Gateway peering

Gateways establish ntied connections between each other (mutual authentication).
Through these connections flows:
- DHT traffic (FindNode, Store, Query)
- Relay traffic (GatewayForward)

---

## 12. Hole Punching via Gateway

### Signaling

```
A (NAT, on GW1)                GW1              B (NAT, on GW1)
   │                            │                    │
   │── HolePunchRequest ──────>│                    │
   │   { target: PeerId(B) }   │                    │
   │                            │── HolePunchNotify─>│
   │                            │   { requester: A   │
   │                            │     addrs: [...] } │
   │                            │                    │
   │                            │<─ HolePunchNotify──│
   │<── HolePunchNotify ───────│   { requester: B   │
   │    { requester: B          │     addrs: [...] } │
   │      addrs: [...] }        │                    │
   │                            │                    │
   │════ raw UDP HolePunch probes (0x03 packets) ═══│
   │                            │                    │
   │◄═══ direct connection (if successful) ════════►│
```

### Result

- Success → add Direct path, switch active, relay stays as fallback
- Failure → session continues over relay, no disruption

---

## 13. Bootstrap

1. Node knows bootstrap gateway address (configured or DNS)
2. Establishes ntied connection with bootstrap GW
3. GatewayRegister → GatewayRegisterAck
4. DhtPublish → publishes PeerId → gateway mapping
5. Receives k-bucket info from GW → discovers other gateways
6. Optionally registers on additional gateways (backup)

---

## 14. Implementation Phases

### Phase 1 — Foundation ✅

| Item | Description |
|------|-------------|
| `Node` API | Rename `Transport` → `Node`, add `connect_addr(SocketAddr)`, `join_network` stub |
| Remove `Packet::Relay` | Dead code, relay is now via frames |
| Discovery update | `resolve` returns `RouteInfo` enum instead of `Option<SocketAddr>` |

**Depends on:** nothing
**Status:** Done. Also added `PeerId::zero()`, `NetworkConfig`, `Node::peer_id()`, `INITIAL_MTU=1350`.

### Phase 2 — Gateway frame types ✅

| Item | Description |
|------|-------------|
| Wire format | Add 7 gateway frames (0x10–0x16) to `wire/frame.rs` with encode/decode |
| Tests | Round-trip encode/decode for each new frame |

**Depends on:** nothing (parallel with Phase 1)
**Status:** Done. Also added `Reader::read_socket_addr` / `Writer::write_socket_addr` to codec. 14 new wire tests.

### Phase 3 — DHT frame types + DhtRecord ✅

| Item | Description |
|------|-------------|
| Wire format | Add 6 DHT frames (0x20–0x25) to `wire/frame.rs` |
| `dht/record.rs` | `DhtRecord`, `GatewayInfo`, `RoutingPolicy` — serialization, signature, verification |
| Tests | Round-trip encode/decode, signature verification |

**Depends on:** nothing (parallel with Phase 1 and 2)
**Status:** Done. DhtPublish/DhtQueryReply/DhtStore use fragment_index/fragment_total for large records. 6 tests in `dht/tests.rs`.

### Phase 4 — Transport path abstraction ✅

| Item | Description |
|------|-------------|
| `TransportPath` | `ConnEntry.path` replaces `peer_addr` |
| Unhandled frames | `PeerConnection::on_data_packet` returns `Vec<Frame>` for gateway/DHT frames |
| Relay send | When path is `Relayed`, wrap packets in GatewayRelay and send through gateway connection |
| Relay receive | Extract inner packet from GatewayDeliver, feed to appropriate PeerConnection |

**Depends on:** Phase 1, Phase 2
**Status:** Done. `is_connection_frame()` splits local vs external frames. `send_packets()` handles both Direct and Relayed. `Connection::transport_path()` exposed. 2 new net tests.

### Phase 5 — Gateway client ✅

| Item | Description |
|------|-------------|
| `gateway/client.rs` | Connect to GW, send GatewayRegister, handle GatewayRegisterAck |
| Relay send/recv | Send GatewayRelay, receive GatewayDeliver, dispatch to E2E sessions |
| HolePunch request | Send HolePunchRequest, handle HolePunchNotify |
| Integration | Wire into `recv_loop` and `flush_all` for gateway frame handling |
| `join_network` | Implement: connect to bootstrap → register → ready |

**Depends on:** Phase 2, Phase 4
**Status:** Done. Gateway client logic is in `api.rs` (not a separate module — tightly coupled with TransportState).
Key functions: `join_network`, `connect_via_relay`, `process_unhandled_frame`, `process_gateway_deliver`,
`handle_key_exchange_init_relayed`, `process_relayed_data`. Two-pass flush in `flush_all`.
**Fix applied:** `std::mem::forget(gw_conn)` in `join_network` to prevent Connection drop from closing gateway connection.

### Phase 6 — Gateway server ✅ (code done, integration test has a bug)

| Item | Description |
|------|-------------|
| `gateway/server.rs` | Accept connections, handle GatewayRegister |
| `gateway/routing.rs` | Local client table, route relay traffic (GatewayRelay → GatewayDeliver) |
| Single-GW routing | Route between locally registered clients |
| Integration test | Two clients behind simulated NAT communicate through gateway |

**Depends on:** Phase 2, Phase 4
**Status:** Server logic is in `api.rs` via `Node::enable_gateway()` + `process_gateway_server_frame`.
Handles GatewayRegister (register client, send ack), GatewayRelay (route to dest, send GatewayDeliver),
HolePunchRequest (send HolePunchNotify to target). Routing table: `gateway_clients: HashMap<PeerId, RegisteredClient>`.

**🔴 Known bug in `relay_through_gateway` integration test:**
The relay routing works — debug output confirms KEInit (1258 bytes), KEResponse (1137 bytes),
and Auth fragments (~1138 bytes each, 5-6 fragments) are all delivered via GatewayRelay → GatewayDeliver.
However, the E2E session auth never completes (handshake times out after 10s).

**Debugging context for next session:**
- Registration works: both clients register, GatewayRegisterAck sent/received.
- GatewayRelay routing works: gateway forwards inner blobs to correct destination.
- GatewayDeliver processing works: clients receive and decode inner packets (KEInit, KEResponse, Data).
- `process_gateway_deliver` dispatches to `handle_key_exchange_init_relayed` / `handle_key_exchange_response` / `process_relayed_data`.
- **Likely root cause:** `process_relayed_data` feeds inner Data packets to the E2E session's `on_data_packet`,
  but the E2E session may not be processing auth fragments correctly. Possibilities:
  1. Auth fragments arrive but are being fed to the wrong session (session ID mismatch between
     inner Data packet's `receiver_session_id` and E2E connection's `local_session_id`).
  2. Auth fragments are processed but AuthComplete never fires because the relayed auth response
     packets aren't being flushed back through the relay (flush timing issue in `process_relayed_data`
     — it doesn't flush the E2E connection, relies on `flush_all` timer at 50ms intervals).
  3. Counter/epoch mismatch: the inner Data packets use the E2E session's counter space, but
     something about how they're created/encrypted/decrypted differs from the direct path.
- **Next step:** Add eprintln in `process_relayed_data` to check if `on_data_packet` successfully
  decrypts and returns frames vs returns empty (decryption failure). If decryption fails, the issue
  is in session key mismatch. If it succeeds but auth doesn't complete, check AuthState assembly.

---

**⏸ POC Checkpoint: Relay partially works (Phases 1–6)**

Gateway registration, relay routing, KEInit/KEResponse/Auth delivery — all working.
E2E session establishment over relay has a bug that needs debugging.
230 unit/codec tests pass. `connect_addr` integration test passes.

---

### Phase 7 — DHT core

| Item | Description |
|------|-------------|
| `dht/kademlia.rs` | K-buckets, XOR distance, node management, bucket refresh |
| `dht/protocol.rs` | FindNode/Reply handlers, iterative lookup, Publish/Query/Store |
| Unit tests | Bucket operations, distance calculations, lookup convergence |

**Depends on:** Phase 3 (can start in parallel with Phases 5–6)
**Estimate:** 4–5 days

### Phase 8 — DHT integration

| Item | Description |
|------|-------------|
| Gateway: DHT node | Gateway handles DHT frames from clients and other gateways |
| `DhtDiscovery` | Implements `Discovery` trait using DHT lookup |
| `Node::connect` | Resolve via DHT → get gateway info → relay or direct |
| `Node::join_network` | After register: DhtPublish to gateway |
| Integration test | Peer A finds peer B via DHT, connects through gateway |

**Depends on:** Phase 5, Phase 6, Phase 7
**Estimate:** 2–3 days

### Phase 9 — Hole punch via gateway signaling

| Item | Description |
|------|-------------|
| HolePunch flow | HolePunchRequest → GW → HolePunchNotify → raw UDP probes |
| Integration | Wire into connection flow: relay → hole punch attempt → direct upgrade |
| Fallback | If hole punch fails, session stays on relay |
| Integration test | Two NATed peers, hole punch succeeds, traffic switches to direct |

**Depends on:** Phase 5, Phase 8
**Estimate:** 2–3 days

### Phase 10 — Multi-path & failover

| Item | Description |
|------|-------------|
| `SessionPaths` | Session tracks multiple paths with RTT |
| Path switching | Automatic failover on path death (3 missed pings) |
| Direct upgrade | Relay → direct when hole punch succeeds mid-session |
| Relay optimization | Switch to shorter relay path (shared GW) when discovered |
| Path negotiation frames | PathSuggest (0x30), PathSuggestAck (0x31) |
| Integration test | Connection survives path failure, upgrades to better path |

**Depends on:** Phase 4, Phase 9
**Estimate:** 3–4 days

### Phase 11 — Multi-gateway forwarding

| Item | Description |
|------|-------------|
| GatewayForward | Route between gateways (GW1 → GW2) |
| Reverse route cache | Remember source routes from GatewayForward |
| Gateway peering | Gateways establish connections to each other |
| Integration test | A on GW1, B on GW2, communicate through gateway chain |

**Depends on:** Phase 6, Phase 8
**Estimate:** 2–3 days

### Phase 12 — Private server (future)

| Item | Description |
|------|-------------|
| `GatewayAuth` trait | Authorization hook for gateway registration |
| Auth in GatewayRegister | `auth_data` field processing |
| `RoutingPolicy` | GatewayRestricted in DhtRecord |
| PathSuggest with auth_token | Peer shares server credentials for path switch |

**Depends on:** Phase 10, Phase 11
**Estimate:** 2–3 days

---

## 15. Summary Timeline

```
Week 1:  Phase 1 + 2 + 3 (foundation + all frame types)
Week 2:  Phase 4 + 5 + 6 (transport path + gateway client/server)
         ──── Relay POC ────
Week 3:  Phase 7 + 8 (DHT core + integration)
         ──── DHT POC ────
Week 4:  Phase 9 + 10 + 11 (hole punch + multi-path + multi-GW)
         ──── Full POC ────
Later:   Phase 12 (private server)
```

Phases 1, 2, 3 are independent and can run in parallel.
Phase 7 (DHT core) can start in parallel with Phases 5–6.
