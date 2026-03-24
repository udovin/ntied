# Stack Overflow — Root Cause & Fix Plan

## Problem

Post-quantum cryptographic types from `ml-dsa` and `ml-kem` have large internal
state. When these types appear as local variables in `async` functions, they
become part of the future's state machine. In debug builds (no optimizations,
no inlining), the compiler preserves every intermediate value, inflating futures
to tens of kilobytes. Tokio worker threads with default stack sizes (~2 MB)
overflow when polling these futures.

## Type Sizes

| Type                 | Approximate Size | Source                        |
|----------------------|------------------|-------------------------------|
| `PrivateKey`         | ~6 KB            | ML-DSA-65 SigningKey + VerifyingKey |
| `EphemeralPrivateKey`| ~3 KB            | ML-KEM-768 DecapsulationKey   |
| `PublicKey`          | 1984 B           | ML-DSA-65 VerifyingKey        |
| `Signature`          | 3373 B           | ML-DSA-65 Signature           |
| `EphemeralPublicKey` | 1216 B           | ML-KEM-768 EncapsulationKey   |
| `KemCiphertext`      | 1120 B           | ML-KEM-768 Ciphertext         |

Additionally, ML-DSA key generation, signing, and verification internally
allocate large polynomial matrices on the call stack (~30–100 KB of temporaries
per operation). These are not visible in our code but contribute significantly
to stack depth.

## Current Workarounds

### 1. Custom iced Executor (`ntied/src/main.rs`)

iced creates a tokio runtime via `tokio::runtime::Runtime::new()` which uses
the OS default thread stack size. On Windows, even the PE header override does
not help because the runtime is constructed before any threads inherit the
setting reliably.

The workaround: a custom `Executor` that builds the tokio runtime with an
explicit `thread_stack_size(16 MB)`.

```rust
struct Executor(tokio::runtime::Runtime);

impl iced::Executor for Executor {
    fn new() -> Result<Self, std::io::Error> {
        tokio::runtime::Builder::new_multi_thread()
            .thread_stack_size(16 * 1024 * 1024)
            .enable_all()
            .build()
            .map(Executor)
    }
    // ...
}
```

### 2. PE header stack reserve (`ntied/build.rs`)

Sets `SizeOfStackReserve = 64 MB` in the PE header via `/STACK:67108864`.
Affects the main thread and any thread created with `dwStackSize = 0`.
Serves as a safety net for threads not covered by the custom Executor.

### 3. Test helper `run_async` (`tests/v2_api_tests.rs`, etc.)

Integration tests spawn a dedicated thread with 16 MB stack and build a tokio
runtime with matching `thread_stack_size`.

### 4. Box-allocation in `api.rs`

`EphemeralPrivateKey` and `PeerConnection` are heap-allocated before being
stored:

```rust
let eph = Box::new(EphemeralPrivateKey::generate());
let conn = Box::new(PeerConnection::new(...));
```

## Hot Spots to Fix

### A. `build_auth_payload` (`api.rs`)

Creates `PublicKey` (1984 B) and `Signature` (3373 B) on the stack. Called
from `handle_key_exchange_init` and `handle_key_exchange_response` — both are
async functions, so these values inflate the future.

```rust
fn build_auth_payload(identity: &PrivateKey, transcript_hash: &[u8]) -> Vec<u8> {
    let pk = identity.public_key();   // 1984 B on stack
    let sig = identity.sign(transcript_hash); // 3373 B on stack + ML-DSA internals
    // ...
}
```

### B. `handle_key_exchange_init` (`api.rs`)

- `EphemeralPrivateKey::generate()` — Box-allocated (good), but `generate()`
  itself creates the key on the stack before returning, and ML-KEM key
  generation uses large temporaries internally.
- `encapsulate()` produces `KemCiphertext` (1120 B) and `SharedSecret` (64 B)
  on the stack.
- `EncryptionKeys::new()` takes references (good), but internally runs
  HKDF-Expand which is lightweight.
- `KeyExchangeResponse` contains `KemCiphertext` (1120 B) on the stack.
- `Session::new()` takes `EncryptionKeys` by value — two `EncryptionKey` (32 B
  each), manageable.

### C. `handle_key_exchange_response` (`api.rs`)

- `decapsulate()` runs ML-KEM decapsulation with large temporaries.
- `EphemeralPublicKey` (1216 B) reconstructed on the stack via
  `pending.ephemeral_key.public_key()`.

### D. `Transport::connect` (`api.rs`)

- `EphemeralPrivateKey::generate()` — Box-allocated (good).
- `KeyExchangeInit` contains `EphemeralPublicKey` (1216 B) on the stack.

### E. `PrivateKey::generate` / `PrivateKey::from_bytes` (`crypto/identity.rs`)

- `MlDsa65::key_gen()` allocates polynomial matrices on the stack internally.
  This is inside the `ml-dsa` crate and cannot be Box-allocated from our code.
- `PrivateKey` (~6 KB) is returned by value. Callers that store it in an async
  function inflate the future by 6 KB.

### F. `PrivateKey::sign` / `PublicKey::verify` (`crypto/identity.rs`)

- ML-DSA signing and verification use large stack-allocated intermediaries
  inside the `ml-dsa` crate. These are the deepest contributors to stack
  usage and are outside our direct control.

## Fix Strategy

### Phase 1 — `spawn_blocking` for crypto operations

Move all CPU-intensive and stack-heavy crypto operations into
`tokio::task::spawn_blocking`. This solves two problems simultaneously:

1. **Stack**: blocking threads have their own stack (configurable via
   `max_blocking_threads` / OS default), independent of async worker threads.
2. **Latency**: key generation, signing, and verification are CPU-bound.
   Running them on async workers blocks the event loop.

Target functions to wrap:

| Call site                          | Operation                        |
|------------------------------------|----------------------------------|
| `Transport::connect`               | `EphemeralPrivateKey::generate()` |
| `handle_key_exchange_init`         | `EphemeralPrivateKey::generate()`, `encapsulate()`, `build_auth_payload()` |
| `handle_key_exchange_response`     | `decapsulate()`, `build_auth_payload()` |
| `Transport::bind` (via `init`)     | `identity.public_key().peer_id()` (lightweight, optional) |
| `Session` internals (auth verify)  | `PublicKey::verify()` |

Example transformation for `handle_key_exchange_init`:

```rust
// Before (all on async worker stack):
let resp_eph = Box::new(EphemeralPrivateKey::generate());
let (ct, resp_ss) = resp_eph.encapsulate(&init.ephemeral_public_key).unwrap();
let keys = EncryptionKeys::new(&resp_ss, &init.ephemeral_public_key, &ct);
let auth_payload = build_auth_payload(&shared.identity, &th);

// After (crypto on blocking thread):
let init_eph_pk = init.ephemeral_public_key;
let identity = shared.identity.clone();
let (resp_eph, ct, resp_ss, keys, th, auth_payload) =
    tokio::task::spawn_blocking(move || {
        let resp_eph = Box::new(EphemeralPrivateKey::generate());
        let (ct, resp_ss) = resp_eph.encapsulate(&init_eph_pk).unwrap();
        let keys = EncryptionKeys::new(&resp_ss, &init_eph_pk, &ct);
        let th = compute_transcript_hash(&init_eph_pk, &ct);
        let auth_payload = build_auth_payload(&identity, &th);
        (resp_eph, ct, resp_ss, keys, th, auth_payload)
    })
    .await
    .unwrap();
```

This requires `PrivateKey: Clone` (already implemented) and the large types
to be `Send` (they are).

### Phase 2 — Box-allocate remaining hot spots

For values that stay in async functions after Phase 1 (e.g., `KeyExchangeInit`,
`KeyExchangeResponse`), Box-allocate them to keep the future state small:

```rust
let init = Box::new(KeyExchangeInit { ... });
let response = Box::new(KeyExchangeResponse { ... });
```

### Phase 3 — Verify and remove workarounds

1. Build in debug mode.
2. Run `ntied` without the custom Executor (use default `Runtime::new()`).
3. Run integration tests without `run_async` (use standard `#[tokio::test]`).
4. If no overflow → remove:
   - Custom `Executor` in `main.rs` (revert to default iced executor)
   - `/STACK:67108864` from `build.rs`
   - `run_async` helpers from test files
5. If overflow persists in specific paths → add targeted `spawn_blocking` or
   `Box::pin` for those paths and repeat.

### Phase 4 — Long-term: upstream improvements

- Monitor `ml-dsa` and `ml-kem` crates for heap-allocated internals or
  `#[inline(never)]` annotations that reduce stack usage.
- Consider contributing patches upstream to Box-allocate large temporaries
  inside key generation and signing.

## Verification

To measure future sizes in debug builds:

```rust
fn print_future_size<F: std::future::Future>(name: &str, _f: &F) {
    println!("{}: {} bytes", name, std::mem::size_of_val(_f));
}
```

Call this on the futures returned by `handle_key_exchange_init`,
`handle_key_exchange_response`, `Transport::connect`, etc. before and after
each fix phase. Target: no individual future exceeds ~10 KB.

To check thread stack usage on Windows, set `RUST_MIN_STACK` environment
variable or use tools like Application Verifier to detect stack overflows
with smaller guard pages.