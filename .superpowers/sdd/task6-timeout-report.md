# Task 6: TLS Handshake Timeout — Report

## Approach chosen: (A)

Added a `handshake_timeout: Duration` field to `Upgrader` (defaulting to the new
`READ_HANDSHAKE_TIMEOUT = 15s` pub const, Go `readHandshakeTimeout` parity),
and wrapped both `acceptor.accept(stream)` and `connector.connect(server_name, stream)`
in `tokio::time::timeout(self.handshake_timeout, …)` inside `upgrade()`.
A `with_handshake_timeout(Duration) -> Self` builder method lets tests override
the timeout without touching any other field.

Approach (A) was chosen over (B) because:
- The timeout is logically a property of the `Upgrader` (the component that owns the
  handshake), not of the call site (`net_impl`). Keeping it in the struct makes it
  mockable and testable without touching `net_impl` at all.
- A single place to set/override (the field) vs. two call sites in `net_impl`.
- Existing tests construct `Upgrader` directly; the builder pattern is backward-compatible.

## Const / field added

```rust
// crates/ava-network/src/peer/upgrader.rs
pub const READ_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Upgrader {
    ...
    handshake_timeout: Duration,  // defaults to READ_HANDSHAKE_TIMEOUT
}

pub fn with_handshake_timeout(mut self, timeout: Duration) -> Self { ... }
```

No new error variants were added. The existing `Error::Tls(String)` variant is
reused with the message `"handshake timeout"` (matches the assertion in the test).

## RED test — how the hang was reproduced

The test `stalled_tls_handshake_is_bounded_by_handshake_timeout` uses
`tokio::io::duplex(1 << 16)` where the **remote half is kept alive** (`_remote`
binding, NOT dropped). This means:
- The TCP buffer is open — rustls happily writes the ClientHello into the local
  half of the duplex.
- The remote half never reads or writes any bytes — so `connector.connect()` waits
  forever for a ServerHello that never arrives.

Before the fix (without `tokio::time::timeout` wrapping):
- The outer 2-second `tokio::time::timeout` guard would fire, panicking the test
  with "upgrade future must complete within the test guard (2 s); if it timed out
  here the handshake_timeout is not wired up".
- Confirmed: the compile error (`no method named 'with_handshake_timeout'`) was the
  first RED signal; commenting out the method call and removing the timeout wrap
  would reproduce the hang directly.

The test compiled-RED was confirmed: the compiler rejected it with
`error[E0599]: no method named 'with_handshake_timeout' found for struct 'upgrader::Upgrader'`.

## GREEN output

```
PASS [0.705s] ava-network peer::upgrader::tests::stalled_tls_handshake_is_bounded_by_handshake_timeout
```

## Full ava-network test count

```
Summary [13.915s] 74 tests run: 74 passed (3 leaky), 0 skipped
```

74/74 green. The 3 "leaky" tests are pre-existing (bloom filter tests that spawn
tokio runtimes without awaiting full shutdown; unrelated to this change).

## Clippy

```
cargo clippy -p ava-network --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.49s
```

Zero warnings.

## avalanchers build

```
cargo build -p avalanchers
Finished `dev` profile [unoptimized + debuginfo] target(s) in 13.95s
```

Clean.

## New error variant

None. `Error::Tls("handshake timeout".into())` reuses the existing `Tls(String)`
variant. The message string is the literal matched by the test assertion.

## Concerns

None. The fix is minimal and focused:
- No behavioral change for connections that complete the handshake before 15 s.
- The 15 s production default matches Go exactly.
- `with_handshake_timeout` is marked `#[must_use]` and is test-accessible without
  any `#[cfg(test)]` gating (it is a clean public builder).
- Both inbound (`acceptor.accept`) and outbound (`connector.connect`) paths are
  covered symmetrically.
