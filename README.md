# crossbeam-channel Actor Model PoC

A small actor runtime built on `crossbeam-channel`'s MPMC channels, with **typed
messages, request/response, backpressure, graceful shutdown, and panic
classification**. Public-API errors follow the
[`rust-error-handling`](https://github.com/libkluid/.skills) skill from
`libkluid/.skills`: each boundary owns its failures, classifies them, and the
binary reports them exactly once.

- Verified with Rust 1.100.0 / edition 2024 / `resolver = "3"`
- `crossbeam-channel` 0.5.17, `thiserror` 2.0.21, `anyhow` 1.0.104
- `cargo test --workspace` passes 30 tests; clippy, rustfmt, and rustdoc are clean

## What It Demonstrates

| Topic | Implementation |
| --- | --- |
| Actor isolation | One OS thread per actor; state is reachable only through `&mut self`, so no locks are required |
| Typed messages | `Actor::Message` associated type; `Address<A>` rejects messages for another actor at compile time |
| Request/response | `Reply<T>` travels inside the message; `ask`/`ask_timeout` wait for the answer |
| Backpressure | Bounded mailbox by default (1024); `tell` blocks, `try_tell` reports `MailboxFull` |
| Graceful shutdown | `Stop` is queued at the FIFO tail; `ActorRef::shutdown` also joins the thread |
| Handler control | `Context::stop()` exits after the current message |
| Panics | Thread unwinding is classified as `Error::Panicked` by `JoinHandle::join()` |
| Domain failures | Returned as `Reply<Result<T, E>>`; the actor stays alive and callers can match the type |
| Error classification | `actor::Error::kind()`, `is_retryable()`, and `sysexits.h`-style exit codes |

## Structure

```text
actors-demo (bin)          argv, scenarios, output, exit codes (anyhow)
  ├── counter (lib)        counter actor + domain error (thiserror)
  └── actor (lib)          actor runtime + runtime error (thiserror)
        └── crossbeam-channel
```

| Crate | Responsibility | Error type | Classification | Consumers |
| --- | --- | --- | --- | --- |
| `actor` | Mailbox, send/receive, lifecycle | `actor::Error` | `kind()`, `is_retryable()` | `counter`, `actors-demo` |
| `counter` | Counter state and domain rules | `counter::Error` (reply payload) | variant | `actors-demo` |
| `actors-demo` | argv, execution, reporting | `UsageError` + `anyhow` | exit code | user/shell |

Dependencies point inward (`actor`), and error conversion happens only in outer
layers. There is no shared `errors` crate and no workspace-wide god enum.

## Quick Start

```bash
cd actors
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Several producer threads update one counter actor.
cargo run -p actors-demo -- run --producers 4 --per-producer 2000 --capacity 64
```

`run` sends 4 producers × 2,000 increments with `tell`, then reads the total with
`ask`. When the bounded mailbox fills up, producers block, so memory does not
grow without bound.

## Failure Scenarios

| Command | Failure being demonstrated | Exit code |
| --- | --- | --- |
| `fail` | Handler replies with `counter::Error::Overflow` | 65 `EX_DATAERR` |
| `panic` | Handler panic → `Error::Panicked` | 70 `EX_SOFTWARE` |
| `timeout` | 250 ms handler awaited by a 50 ms `ask_timeout` | 75 `EX_TEMPFAIL` |
| `saturate` | One-slot mailbox filled, then `try_tell` | 75 `EX_TEMPFAIL` |
| `stopped` | `tell` to an actor that already exited | 69 `EX_UNAVAILABLE` |
| (argument error) | Missing/unknown command, zero value | 2 (usage) |

```bash
cargo run -p actors-demo -- fail      # exit 65
cargo run -p actors-demo -- panic     # exit 70
cargo run -p actors-demo -- timeout   # exit 75
cargo run -p actors-demo -- saturate  # exit 75
cargo run -p actors-demo -- stopped   # exit 69
cargo run -p actors-demo --           # exit 2, prints usage
```

Example output for `timeout`:

```text
error: could not run the timeout scenario

Caused by:
    0: the slow request did not finish in time
    1: actor `chaos` did not send a reply within 50ms
```

Example output for `fail`:

```text
error: could not run the domain error scenario

Caused by:
    0: the counter rejected the increment
    1: counter overflow: 1 + 18446744073709551615 exceeds 18446744073709551615
```

## Actor Model at a Glance

### Message Flow

```text
caller                        mailbox                  actor thread
  │ ask_timeout(build)           │                         │
  │ build(Reply<T>) → Message    │                         │
  ├──── send_timeout ───────────►│──── recv ──────────────►│ handle(&mut self, msg)
  │                              │                         │ reply.send(value)
  │◄──────── recv_timeout ───────┼──── reply channel ──────┤
```

- `ask` creates a one-slot reply channel and embeds its sender as `Reply<T>` in
  the message. The reply type is fixed by the message field, so there is no
  runtime downcasting.
- The `ask_timeout` deadline covers **both mailbox wait and reply wait**.
- If a handler drops the `Reply`, the caller receives `Error::NoReply`. This is
  distinct from a timeout: the request arrived, but no answer was produced.

### Shutdown Paths

| Method | Meaning | Joins thread |
| --- | --- | --- |
| `Address::stop()` | Process every already-sent message, then exit | No |
| `ActorRef::shutdown()` | `stop()` plus thread join | Yes |
| `Context::stop()` | Finish the current message, drop the rest of the queue | No |
| Last `Address` dropped | Channel disconnect → drain remaining queue, then exit | Yes, if `JoinHandle` is retained |
| Handler panic | Immediate unwinding; queue and pending replies are dropped | Reported as `Error::Panicked` |

`Stop` is a normal FIFO message, not a priority signal. Therefore
`shutdown()` is a **drain-first** shutdown. By contrast, `Context::stop()` is a
**stop-now** shutdown that discards work queued after the current message.

### Panics

When an actor thread unwinds, its receiver is dropped. Pending requests in the
queue lose their `Reply` handles and waiting callers receive `NoReply`. If the
`JoinHandle` was retained, `join()` converts the panic payload to text and
returns `Error::Panicked`. The runtime does not restart actors automatically;
supervision trees are an extension point.

## Error Handling (Skill Applied)

### 1. Ownership per Boundary

- `actor::Error` represents **mailbox/lifecycle** failures only. It knows
  nothing about domain failures.
- `counter::Error` represents **domain rule** failures only. It knows nothing
  about mailbox or thread failures.
- `actors-demo` uses `UsageError` (arguments) and `anyhow` (context), then
  downcasts typed errors into exit codes.

### 2. Typed Libraries, Contextual Binaries

```rust
// Public library API: callers can match on a type.
pub fn spawn(capacity: NonZeroUsize) -> actor::Result<ActorRef<Counter>>;

// Binary internals: accumulate the narrative of what failed.
let total = counter
    .ask(Message::Value)
    .context("could not read the final counter value")?;
```

No `anyhow::Error`, `Box<dyn Error>`, or `String` appears in a public library
API. Both `actor` and `counter` provide a `pub type Result<T>` alias.

### 3. Classification vs. Policy

```rust
pub fn kind(&self) -> ErrorKind;      // stable classification
pub fn is_retryable(&self) -> bool;   // a hint; policy belongs to the consumer
```

| `ErrorKind` | Retry hint | Exit code | Meaning |
| --- | --- | --- | --- |
| `Timeout` | Yes | 75 | Mailbox or reply deadline exceeded |
| `Overloaded` | Yes | 75 | Bounded mailbox is full |
| `NoReply` | No | 70 | Request delivered, but reply handle dropped |
| `Panicked` | No | 70 | Actor thread panicked |
| `Stopped` | No | 69 | Send attempted after the actor exited |
| `Spawn` | No | 71 | OS thread creation failed |

Even though `Timeout` and `Overloaded` are marked retryable, nothing retries
automatically: a non-idempotent message may be processed twice. Retry policy is
the consumer's responsibility.

### 4. Source Chains Without Duplicate Output

```rust
#[error("could not start actor `{actor}`")]
Spawn {
    actor: &'static str,
    #[source]
    source: io::Error,
}
```

`Display` states only what failed at this layer; the lower-level cause lives in
`#[source]`. Values such as the timeout are preserved as structured fields
(`operation`, `timeout`) instead of being flattened into strings. The binary
prints the context and `Caused by:` chain once with `error: {err:?}`.

### 5. Panics and Exit Codes

- Recoverable failures return `Result`; programmer mistakes panic.
- Workspace lints warn on `unwrap`/`expect`/`panic`; allowances are limited to
  tests and the intentionally crashing `chaos` actor.
- The `panic` scenario temporarily disables the default panic hook so the
  payload captured by `JoinHandle` is reported exactly once at the top level.
- Usage errors print usage and exit with code 2 instead of producing an error
  chain.

### 6. Public API Hygiene

- `#[non_exhaustive]` on every public error enum
- `Debug + Display + std::error::Error + Send + Sync + 'static`
- Rustdoc `# Errors` sections and per-variant documentation
- `unsafe_code = "forbid"` and `missing_docs` warnings

## Verification

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

Test layout:

| Scope | Count | Coverage |
| --- | --- | --- |
| `actor` unit | 4 | Display, kind/retryable, source chain, `Send + Sync` |
| `actor` integration | 11 | FIFO, ask, timeout, `NoReply`, overload, stopped, panic, drain, disconnect, context stop, unbounded |
| `counter` unit | 5 | Serialization, domain error, survival after rejection, Display, name |
| `actors-demo` CLI | 9 | Success path, exit codes 65/69/70/75, usage exit 2, panic reported once |
| Doctest | 1 | Public API usage |
| **Total** | **30** | |

## Limitations and Extension Points

- **No supervision or restart**: a panic is reported and the actor is gone.
  Restart policy (one-for-one, backoff, maximum attempts) belongs to a higher
  layer.
- **Handlers are synchronous and blocking**: calling `ask` from a handler back
  into the same actor deadlocks. Cyclic synchronous requests between actors have
  the same problem.
- **At-most-once delivery**: there is no automatic retry. Idempotency keys and
  deduplication are application concerns.
- **One OS thread per actor**: when actor count exceeds available cores, use a
  thread pool or an async bridge.
- **Local only**: no remote transport, serialization, actor registry, or
  distributed supervision.
- **Timeouts cancel only the caller**: a running handler is not interrupted.
- **One-slot reply channel**: `Reply` can send once, enforced by ownership.

Suggested extension order: supervision tree → `crossbeam_channel::select!` loop
for multiple addresses and timers → tracing fields → actor registry → async
adapter → remote transport.

See **[docs/design.md](docs/design.md)** for design decisions and the full test
matrix.

## Files

```text
actors/
├── Cargo.toml
├── README.md
├── docs/design.md
├── crates/
│   ├── actor/                 # actor runtime
│   │   ├── src/{lib,address,error,reply,runtime}.rs
│   │   └── tests/actor.rs
│   ├── counter/               # domain actor
│   │   └── src/lib.rs
│   └── demo/                  # CLI scenarios
│       ├── src/{main,args,chaos,scenarios}.rs
│       └── tests/cli.rs
└── .opencode/skills/rust-error-handling/SKILL.md
```