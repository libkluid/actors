# Design Notes: A crossbeam-channel Actor Runtime

This document records the design decisions, concurrency contracts, error model,
and test matrix of the PoC. See the [README](../README.md) for usage.

Verified with Rust 1.100.0 / edition 2024 / `crossbeam-channel` 0.5.17.

---

## 1. Goals and Non-goals

### Goals

1. Isolate actor state on a single thread and mutate it only through messages.
2. Guarantee message and reply types at compile time.
3. Provide backpressure through bounded mailboxes.
4. Distinguish graceful shutdown (drain-first, stop-now) from panic failure.
5. Separate runtime failures from domain failures at distinct type boundaries.
6. Demonstrate the rules of the `rust-error-handling` skill in real code and
   tests.

### Non-goals

- Supervision trees and automatic restarts
- Remote actors, serialization, or service discovery
- async/await integration
- Cancellation of a running handler
- exactly-once delivery

The PoC focuses on documenting these boundaries and leaving explicit extension
points.

---

## 2. Crate Topology

```text
actors-demo (bin)
  │  argv, scenario composition, output, exit codes
  │  UsageError + anyhow + downcast
  ├──────────────► counter (lib)
  │                  CounterActor, counter::Error
  │                  Reply<Result<u64, counter::Error>>
  └──────────────► actor (lib)
                     Actor, Address, Reply, ActorRef
                     actor::Error + ErrorKind
                       └── crossbeam-channel
```

Dependencies point only from `actors-demo → counter → actor`. `actor` does not
know `counter`, and `counter` does not know `actors-demo`. Error conversion only
happens as outer layers wrap inner errors.

| Boundary | Owns these failures | Does not own |
| --- | --- | --- |
| `actor` | Closed mailbox, deadline, missing reply, overload, panic, thread creation | Domain rule violations |
| `counter` | `u64` overflow | Mailbox or thread failures |
| `actors-demo` | Argument parsing, user messages, exit codes | Detailed causes inside lower layers |

---

## 3. Actor Lifecycle

### 3.1 States

```text
spawn()
  │
  ▼
Started ── started() ──► Running ── handle() loop ──► Stopped ── stopped() ──► Exit
                          │
                          ├── receives Envelope::Stop ────────► Stopped
                          ├── Context::stop() ────────────────► Stopped
                          ├── all senders dropped ────────────► Stopped
                          └── handler panic ──────────────────► Panicked (no stopped())
```

### 3.2 Thread Loop

```rust
let mut ctx = Context::new(actor.name());
actor.started(&mut ctx);
if !ctx.is_stopping() {
    while let Ok(envelope) = receiver.recv() {
        match envelope {
            Envelope::Message(message) => {
                actor.handle(message, &mut ctx);
                if ctx.is_stopping() { break; }
            }
            Envelope::Stop => break,
        }
    }
}
actor.stopped(&mut ctx);
```

Core contracts:

- `handle` runs only on the actor thread, so `&mut self` needs no lock.
- `Context::stop` ends the loop **after the current message**.
- `Stop` is not a priority message; it is a normal FIFO entry.
- Calling `stop` from `started` skips the message loop but still runs
  `stopped`.
- If `handle` panics, `stopped` does not run.

### 3.3 Shutdown Methods

| Method | Queue handling | Waits for thread | Use case |
| --- | --- | --- | --- |
| `Address::stop()` | Process every already-sent message | No | Request shutdown only |
| `ActorRef::shutdown()` | Process everything | Yes | Owner performs cleanup |
| `Context::stop()` | Current message only; drop the rest | No | Handler stops itself |
| Last `Address` dropped | Disconnect, drain remaining queue | If `JoinHandle` is retained | RAII cleanup |
| Panic | Immediate abort, drop queue | If `JoinHandle` is retained | Failure |

`Address::stop()` blocks while a bounded mailbox is full. In other words, a stop
request is subject to backpressure too. `shutdown()` then joins the thread. A
`stop()` call against an already-exited actor returns `Stopped`, but
`shutdown()` treats that as a successful shutdown.

---

## 4. Message and Reply Protocol

### 4.1 Typed Messages

```rust
pub trait Actor: Sized + Send + 'static {
    type Message: Send + 'static;
    fn handle(&mut self, message: Self::Message, ctx: &mut Context<Self>);
}
```

Each actor has its own message enum. `Address<A>` accepts only `A::Message`, so
sending a message to the wrong actor is a compile error. Compared with erasing
messages into `Box<dyn Any>` and downcasting at runtime, this is safer and keeps
message size smaller.

### 4.2 Request/Response

```rust
pub struct Reply<T> { sender: Sender<T> } // bounded(1)

addr.ask(|reply| Message::Get { reply })?;
```

- `ask` creates a `Reply<T>` and passes it to the `build` closure. The type of
  the message field produced by the closure fixes `T`, so the reply type cannot
  drift.
- `Reply::send(self, T)` consumes the handle. Answering one request twice is
  impossible at the type level.
- If the caller leaves first (timeout), `send` returns `ReplyDropped`. Handlers
  may ignore it; this is not an actor failure.
- The reply channel has one slot, so `send` never blocks.

### 4.3 `ask` and `ask_timeout`

| API | Deadline | Failures |
| --- | --- | --- |
| `ask(build)` | None | `Stopped`, `NoReply` |
| `ask_timeout(timeout, build)` | Whole request | Above + `Timeout { operation }` |
| `tell(message)` | None (waits for mailbox space) | `Stopped` |
| `tell_timeout(timeout, message)` | Mailbox space | `Stopped`, `Timeout { operation: Send }` |
| `try_tell(message)` | Immediate | `Stopped`, `MailboxFull` |

`ask_timeout` captures a deadline with `Instant` and spends what remains after
the send on waiting for the reply. Because the stages share one timeout, the
caller gets a total upper bound rather than a separate timeout per stage.

`ask` has no deadline, so a caller can wait forever on a wedged actor.
Production code should generally prefer `ask_timeout`.

### 4.4 Delivery Guarantees

- Channels provide at-most-once delivery. The runtime never resends.
- Messages from one `Address` arrive in FIFO order.
- There is no global ordering guarantee across addresses or threads.
- A successful `tell` means "queued", not "processed".
- If the outcome matters, use `ask` and wait for a reply.

---

## 5. Backpressure and Channel Choice

### 5.1 Why `crossbeam-channel`?

| Requirement | `std::sync::mpsc` | `crossbeam-channel` |
| --- | --- | --- |
| Cloneable senders (MPMC send) | Supported | Supported |
| Bounded channel + `try_send` | Limited | Supported |
| Send/recv timeouts | recv only | Both directions |
| Multi-channel `select!` | No | Yes |
| Capacity/queue inspection | No | `capacity()`, `len()` |

The PoC does not use `select!` yet, but timers, multiple addresses, and shutdown
signals will need it immediately. The distinction between `send_timeout` and
`try_send` also makes the backpressure scenario explicit.

### 5.2 Defaults

- `spawn` → capacity 1024 (bounded)
- `spawn_bounded` → caller-provided capacity
- `spawn_unbounded` → explicit opt-in

Bounded is the default because the failure mode should be **slowness (blocking)**,
not **memory exhaustion**. However, `tell` may block, so a structure where a
producer and an actor wait on each other (for example, a handler joining a
producer) can deadlock.

### 5.3 Meaning of `MailboxFull`

`try_tell` returning `MailboxFull` means the actor is **alive but busy**. It must
be distinguished from `Stopped`:

- `MailboxFull` → `ErrorKind::Overloaded`, retry hint `true`
- `Stopped` → `ErrorKind::Stopped`, retry hint `false`

---

## 6. Panics and Failure Isolation

### 6.1 Panic Path

```text
handle() panics
  ├─ panic hook runs (the demo CLI briefly disables it to avoid duplicate output)
  ├─ thread unwinds
  ├─ receiver drops → queued Envelope/Reply values drop
  │    └─ waiting ask callers: NoReply
  ├─ JoinHandle::join() → Err(payload)
  │    └─ Error::Panicked { actor, panic: String }
  └─ stopped() is not called
```

A panic is not a domain failure; it is a failure of the actor instance. That is
why it is classified through `JoinHandle` rather than `Reply<Result<T, E>>`.
`ErrorKind::Panicked` has a retry hint of `false`, because state may already be
lost and automatic retry is unsafe.

### 6.2 Panic Payload

`JoinHandle::join` extracts `&str`/`String` from `Box<dyn Any + Send>` and stores
it as text. The payload is not a `std::error::Error`, so it is not placed in the
source chain; it is preserved as a field on `Error::Panicked`. This also avoids
printing the same cause twice in both `Display` and `source`, as the skill
requires.

### 6.3 Where Reporting Happens

Actor threads do not log; they return failures as types. User-facing messages and
exit codes are decided only at the top of the binary. The `panic` scenario
temporarily suppresses the default hook so its output does not overlap with the
top-level report.

---

## 7. Error Model

### 7.1 `actor::Error`

```rust
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    Stopped { actor: &'static str },
    Timeout { actor, operation, timeout },
    NoReply { actor },
    MailboxFull { actor },
    Panicked { actor, panic },
    Spawn { actor, #[source] source: io::Error },
}
```

- `actor: &'static str`: names come from `Actor::name()` and are static.
- `operation`: distinguishes `Send` from `Reply`, preserving where the deadline
  expired.
- `timeout: Duration`: the caller's value is preserved instead of flattened into
  a string.
- `Spawn`: the only variant wrapping an external `io::Error` as a source, because
  thread creation failures have an investigation-worthy cause.
- `Panicked`: preserves the payload text as a field.
- Public enum, therefore `#[non_exhaustive]`.

`kind()` keeps consumers from coupling to every variant, and `is_retryable()`
provides a hint rather than policy.

### 7.2 `counter::Error`

```rust
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    #[error("counter overflow: {current} + {amount} exceeds {max}")]
    Overflow { current: u64, amount: u64, max: u64 },
}
```

Domain failures return as reply payloads
(`Reply<Result<u64, Error>>`). Transport succeeded and the actor is still alive,
so this is not a runtime error. A test pins down that the actor remains usable
after a rejected `AddChecked`.

### 7.3 Binary Boundary

```text
run(args) -> anyhow::Result<()>
  ├─ scenario().context(...)  // accumulate "what was being attempted"
  └─ main
       ├─ eprintln!("error: {err:?}")  // context + Caused by, once
       └─ exit_code(&err)              // downcast, then kind → code
```

`exit_code` uses `anyhow::Error::downcast_ref` to see through `.context()`
layers. Policy mapping exists in this one function only.

| Classification | Code |
| --- | --- |
| `UsageError` | 2 |
| `counter::Error` | 65 `EX_DATAERR` |
| `actor::ErrorKind::Stopped` | 69 `EX_UNAVAILABLE` |
| `actor::ErrorKind::NoReply` / `Panicked` | 70 `EX_SOFTWARE` |
| `actor::ErrorKind::Spawn` | 71 `EX_OSERR` |
| `actor::ErrorKind::Timeout` / `Overloaded` | 75 `EX_TEMPFAIL` |
| Anything else | 1 |

### 7.4 Skill Checklist Mapping

| Skill rule | Implementation |
| --- | --- |
| Public libraries use `thiserror` typed errors | `actor::Error`, `counter::Error` |
| Binaries use `anyhow` + context | `demo/src/main.rs`, `scenarios.rs` |
| No `anyhow`/`Box<dyn Error>`/`String` in public APIs | `actor`, `counter` public signatures |
| Per-crate `Result<T>` alias | `actor::Result`, `counter::Result` |
| `From` only at real boundaries | No unnecessary `#[from]`; explicit matching |
| Preserve the source chain | `#[source] io::Error` on `Error::Spawn` |
| Do not duplicate cause in `Display` and `source` | `Spawn` message omits the source |
| `#[non_exhaustive]` on public enums | `Error`, `ErrorKind`, `Operation`, `Message` |
| `kind()` / `is_retryable()` | `actor::Error` |
| Add context at boundaries | `.context()` in the demo |
| Report once at the top | `main`; panic hook suppression |
| Panic only for bugs/invariants | Tests and the `chaos` actor only |
| Exit code mapping | `main::exit_code` |
| Pin contracts with tests | 30 tests |
| Enforce unwrap policy with lints | workspace lints + `-D warnings` |
| Rustdoc `# Errors` | Every public fallible function |
| Documented message style | Lowercase, no trailing period |

---

## 8. Concurrency Contracts and Pitfalls

### 8.1 Safe Patterns

- Multiple producer threads clone an `Address` and use `tell` to submit work.
- Use `ask_timeout` to read state or confirm completion.
- Perform request/response from another actor or an external caller.
- The owner shuts down with `ActorRef::shutdown()`.
- To share actor state, transfer ownership through messages rather than reaching
  for `Arc<Atomic*>`.

### 8.2 Patterns to Avoid

| Pattern | Consequence | Alternative |
| --- | --- | --- |
| `ask` to the same actor from a handler | Deadlock | Read state directly or use a separate message |
| Synchronous `ask` A→B and B→A | Cyclic deadlock | `tell` plus asynchronous reply, or flatten the request |
| Handler joins a producer thread | May deadlock with a bounded mailbox | Join producers first, then `ask` |
| Trusting `ask` forever | Waits indefinitely on a slow actor | Use `ask_timeout` |
| Long I/O inside a handler | Delays every other message | Move I/O to a dedicated actor/thread |
| Unbounded mailboxes by default | Memory exhaustion | Bounded mailbox + backpressure |

### 8.3 Cancellation

`ask_timeout` cancels **only the caller's wait**. The actor's `handle` keeps
running. If work must be cancelled, pass a cooperative cancellation token in a
message, or run the work on a separate thread and collect only the result. This
PoC deliberately stays out of that scope.

---

## 9. Test Matrix

### 9.1 `actor` (Unit + Integration)

| Test | Contract pinned down |
| --- | --- |
| `display_messages_are_stable` | Message text, lowercase, no trailing period |
| `kind_mapping_and_retryability_are_stable` | Classification and retry hints |
| `spawn_preserves_the_source` | Source chain of `Spawn` |
| `error_is_send_sync_static` | Error crosses thread boundaries |
| `tell_messages_are_fifo_and_ask_observes_them` | FIFO + ask |
| `ask_timeout_reports_the_reply_stage` | `Operation::Reply` timeout |
| `dropped_reply_is_reported_as_no_reply` | `NoReply` |
| `full_mailbox_is_reported_as_overload` | `MailboxFull` |
| `send_after_shutdown_is_stopped` | `Stopped` |
| `ask_after_shutdown_is_stopped` | ask after exit |
| `panic_is_classified_by_join` | `Panicked` + payload |
| `shutdown_drains_messages_queued_before_stop` | drain-first |
| `dropping_the_last_address_drains_and_stops_the_actor` | Disconnect drain |
| `context_stop_drops_later_messages` | stop-now |
| `unbounded_mailbox_has_no_capacity` | Unbounded path |

### 9.2 `counter`

| Test | Contract pinned down |
| --- | --- |
| `increments_are_serialized_and_readable` | Value after 1,000 increments |
| `checked_increment_reports_overflow_without_killing_the_actor` | Domain error is payload; actor survives |
| `error_display_is_stable` | Domain message |
| `default_name_is_short_and_explicit` | `Actor::name` |
| `error_is_send_sync_static` | Reply crosses thread boundaries |

### 9.3 CLI

| Test | Expectation |
| --- | --- |
| `concurrent_producers_are_serialized` | exit 0, total 4000 |
| `domain_error_uses_ex_dataerr` | exit 65 |
| `timeout_uses_ex_tempfail` | exit 75 |
| `overload_uses_ex_tempfail` | exit 75 |
| `stopped_actor_uses_ex_unavailable` | exit 69 |
| `panic_uses_ex_software_and_is_reported_once` | exit 70, "panicked" exactly once |
| `missing_command_is_usage_error` | exit 2 |
| `zero_capacity_is_usage_error` | exit 2 |
| `unknown_command_is_usage_error` | exit 2 |

---

## 10. Extension Roadmap

1. **Supervision tree**: retain child `JoinHandle`s and restart from a factory on
   `Panicked`. Keep restart counts and backoff in supervisor policy.
2. **`select!` loop**: process multiple addresses and timers in one actor.
3. **Cancellation tokens**: propagate cooperative cancellation to long-running
   work.
4. **Thread pool / async bridge**: when actor count greatly exceeds core count.
5. **Observability**: expose queue length, message counts, handler latency, and
   restart counts as tracing fields.
6. **Actor registry**: look up addresses by name at a system boundary; the system
   manages address lifetimes and shutdown order.
7. **Remote transport**: message serialization and request IDs. Moving from
   at-most-once to exactly-once requires idempotency keys and a deduplication
   store.

Each extension may add a new `ErrorKind` to `actor::Error`, but consumers can keep
their existing mappings thanks to `#[non_exhaustive]` and `kind()`.