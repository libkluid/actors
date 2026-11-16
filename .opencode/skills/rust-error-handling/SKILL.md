---
name: Rust Error Handling
description: Design, implement, review, and test error handling in Rust libraries, binaries, and cargo workspaces. Use for choosing thiserror vs anyhow, defining per-crate error types, preserving source chains, adding context, classifying retryable errors and exit codes, and replacing stringly-typed or shared god errors.
---

# Rust Error Handling

Procedures and templates for designing, implementing, reviewing, and testing error
handling in Rust libraries, binaries, and cargo workspaces. Nothing here depends on
a specific project or file layout, so this document is sufficient on its own for a
new project or for auditing an existing codebase. External links are for reference;
you can apply the rules and templates below without accessing them.

## Portability

This skill is self-contained at the directory level. Copying `SKILL.md` alone is
enough.

- Project-local: `<repo>/.opencode/skills/rust-error-handling/SKILL.md`
- User-global: `~/.config/opencode/skills/rust-error-handling/SKILL.md`
- Compatible locations: the same structure under `<repo>/.claude/skills/` or
  `~/.agents/skills/`
- The skill ID is the directory name (`rust-error-handling`); the frontmatter
  `name` is only a display label.

Because the document references no repository files, adapting it to a new project
usually means changing only crate names, the MSRV, and the exit-code policy.

## When to use this skill

- Designing error types for a new library or binary
- Fixing `String`, `Box<dyn Error>`, or `anyhow` leaking into a public API
- Untangling error conversions across crate boundaries in a cargo workspace
- Choosing between `thiserror` and `anyhow`
- Reviewing error messages, source chains, exit codes, or retry classification
- Supporting multiple consumers (CLI, HTTP, batch jobs) of one library
- Replacing vague "failed to process" messages that make incidents hard to debug

## Deliverables

A completed task should produce the following:

1. An **error ownership table** per crate or layer
2. An **error type and `Result<T>` alias** for every public boundary
3. A **conversion plan** (`From`/context) for each boundary
4. A **classification and policy mapping** per consumer (`kind`, retry, exit code,
   HTTP status)
5. **Tests** covering variants, sources, exit codes, and corrupt data
6. **Lint settings** that enforce the `unwrap`/`expect`/`panic` policy

---

## 0. Gather context first

Before writing code, determine the following. Do not guess; ask if something is
unclear.

```bash
# Crate graph and dependencies
cargo metadata --no-deps --format-version 1

# Existing error types, unwrap calls, anyhow usage (use grep -R if rg is unavailable)
rg -n "enum .*Error|struct .*Error|anyhow!|unwrap\(|expect\(" --type rust

# MSRV, edition, resolver
rg -n "rust-version|edition|resolver" Cargo.toml crates/*/Cargo.toml
```

Questions to answer:

- Which crates are **public libraries**, and which are **internal implementation
  details or binaries**?
- Who consumes them? (CLI, HTTP API, batch job, external users, tests)
- Where must callers **branch and recover** from a failure?
- Where is a failure only **reported to a human**?
- Does the MSRV support `#[non_exhaustive]`, edition 2024, and `core::error::Error`?
- Are there constraints from async runtimes, `no_std`, FFI, or platform-specific
  exit codes?
- If public error types are already published, what do semver rules allow?

> Never rewrite a published error API in one step. Add the new type, migrate
> boundaries first, then deprecate the old API and remove it at the next major
> version.

---

## 1. Decision tree

```text
Fallible operation
├─ Programmer mistake / internal invariant violation
│  └─ panic!, unreachable!, assert!, expect("why this cannot fail")
└─ Expected runtime failure
   ├─ Caller must act differently depending on the failure
   │  └─ crate-local typed error (thiserror) + variant or kind()
   ├─ Public library API
   │  └─ crate-local typed error (thiserror) + Result<T> alias
   ├─ Application/binary internals
   │  └─ anyhow::Result + .context()
   └─ Top-level boundary (main, HTTP handler, job runner)
      └─ Report once + map to exit code/HTTP status
```

### Quick selection table

| Situation | Choice | Why |
| --- | --- | --- |
| Public library function | `thiserror`-based typed error | Callers can match variants |
| Binary internals | `anyhow` + `.context()` | The "what failed" narrative matters more than the type |
| Recovery per failure kind | Match variants directly | Exact information is required |
| Stable classification only | `kind()` (non-exhaustive enum) | Hides internal variant changes |
| Retry decision | `is_retryable()` | Avoids leaking `io::ErrorKind` upward |
| "Absent" is normal | `Option<T>` | It is not a failure |
| "Absent" is a failure | `Result<T, Error>` + `NotFound` | Preserves the reason and id |
| Process exit | `ExitCode` + `sysexits.h` style | Shells and operators can distinguish causes |
| Usage error | Separate `UsageError`, exit code 2 | Keeps invocation errors separate from runtime failures |
| Validated value | Private fields + constructor/`TryFrom` | Makes invalid states unrepresentable |

### When not to add a new error

- If every failure is handled the same way and callers never branch, do not add
  variants. One variant wrapping a source is enough.
- Do not promote an "absent" value to `NotFound` when `Option` already expresses it
  correctly.
- Do not let a library decide **policy** (retry counts, exit codes, HTTP status).
  Policy belongs to the consumer.

---

## 2. Core rules

1. **Own errors at each boundary.** Do not create a shared `errors`/`common` crate
   or one workspace-wide god enum.
2. **Use `thiserror` for libraries and `anyhow` for binaries** by default.
3. **Never expose `anyhow::Error`, `Box<dyn Error>`, or `String` in a public API.**
4. Give every crate `pub type Result<T> = std::result::Result<T, Error>;`.
5. Add `#[from]`/`From` **only at real boundaries**. If the same source type occurs
   in two places, write the variants explicitly.
6. **Preserve the source chain.** Never use `map_err(|e| Other(e.to_string()))`.
7. Expose a wrapped error through **either `source()` or `Display`, not both**.
   Doing both makes reporters print the same cause twice.
8. Put **`#[non_exhaustive]`** on public error enums.
9. Preserve **structured fields** (paths, ids, input values) instead of flattening
   them into message strings.
10. Provide **`kind()`** for consumer branching and **`is_retryable()`** as a hint
    for transient failures.
11. Add context **at call boundaries** with `.context()`/`.with_context()`.
12. **Log once at the top level.** Libraries should not log and return.
13. Use `unwrap`/`expect` only in tests, examples, and proven invariants. An
    `expect` message should explain **why the call cannot fail at that point**.
14. **Recoverable failures return `Result`; bugs panic.**
15. **Error messages start lowercase and have no trailing period** (Rust API
    Guidelines).

---

## 3. Workflow

### 3.1 Build an error ownership table

Draw the crate graph and record each boundary's error type and consumers.

```text
app (bin)
  └── service          use cases, service::Error + ErrorKind
        ├── domain     domain model, domain::Error (no dependencies)
        └── adapter    external systems, adapter::Error
              └── domain
```

| Crate | Responsibility | Error type | Classification | Consumers |
| --- | --- | --- | --- | --- |
| `domain` | Value validity/invariants | `domain::Error` | variants | `adapter`, `service`, `app` |
| `adapter` | I/O, serialization, external APIs | `adapter::Error` | `is_retryable()` | `service`, `app` |
| `service` | Use-case composition | `service::Error` | `kind()`, `is_retryable()` | `app`, HTTP |
| `app` | argv, output, exit codes | `UsageError` + `anyhow` | exit code | user/shell |

Dependencies point inward. Error conversion points outward: outer layers wrap inner
errors, never the reverse.

### 3.2 Start with domain/library errors

1. List failure conditions and the **units callers handle differently**.
2. Give each unit one variant. Merge failures that are always handled together.
3. Turn message values into fields (path, id, key, input length, and so on).
4. `Display` should state only what failed at this layer. Keep inner causes in
   `#[source]`.
5. Add `#[non_exhaustive]` and documentation.

### 3.3 Design boundary conversions

- Wrap an inner error with `#[error(transparent)] Variant(#[from] Inner)` or with a
  distinct message plus `#[from]`.
- If the same source type appears in multiple variants, use a named `#[source]`
  field instead of `#[from]`.
- `#[from]` implies `source()`; never add `#[source]` to the same field.
- To add context while preserving the type, use `.context()` in the binary.
- If one boundary accepts several error types, add one `#[from]` variant per type
  or convert explicitly with `map_err`.
- **No stringification**: replace `map_err(|e| Error::Other(e.to_string()))` with a
  source-preserving variant.

### 3.4 Add classification APIs

Add `kind()` when variants grow or when multiple consumers exist.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    InvalidInput,
    NotFound,
    Conflict,
    Storage,
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::NotFound { .. } => ErrorKind::NotFound,
            Self::Domain(_) => ErrorKind::InvalidInput,
            Self::Store(_) => ErrorKind::Storage,
        }
    }
}
```

Retryability is only a **hint**.

```rust
impl Error {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Store(err) => err.is_retryable(),
            Self::NotFound { .. } | Self::Domain(_) => false,
        }
    }
}
```

Actual retries (backoff, maximum attempts, idempotency checks) are service or caller
policy. Never retry a non-idempotent operation automatically.

### 3.5 Report at the top-level boundary

- Return success or failure as `ExitCode`.
- Print the chain with `anyhow`'s `{:?}` (multi-line with `Caused by:`) or `{:#}`
  (single line).
- For usage errors, print usage instead of an error chain and exit with code 2.
- Log once here, with structured fields.
- For HTTP, map `kind()` to a status code and problem details.

### 3.6 Finish with tests, lints, and docs

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

- Add a rustdoc `# Errors` section to every public fallible function.
- Add tests for new variants, exit codes, and source chains.
- Promote lints such as `unwrap_used` to `-D warnings` in CI.

---

## 4. Copy-paste templates

The code below works in any library or binary after renaming the types.

### 4.1 Domain/library error

```rust
// src/lib.rs
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

/// Failure while constructing a validated user name.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// The name consisted only of whitespace.
    #[error("name must not be blank")]
    BlankName,

    /// The name exceeded the maximum length.
    #[error("name is too long: {len} characters (max {max})")]
    NameTooLong {
        /// Actual character count.
        len: usize,
        /// Accepted maximum.
        max: usize,
    },
}

/// A user name that has passed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    name: String,
}

impl User {
    pub fn new(name: &str) -> Result<Self> {
        let name = name.trim();
        let len = name.chars().count();
        if len == 0 {
            return Err(Error::BlankName);
        }
        if len > 120 {
            return Err(Error::NameTooLong { len, max: 120 });
        }
        Ok(Self {
            name: name.to_owned(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn blank_name_is_rejected() {
        let err = User::new("  ").unwrap_err();
        assert!(matches!(err, Error::BlankName));
        assert_eq!(err.to_string(), "name must not be blank");
    }

    #[test]
    fn error_is_send_sync_static() {
        fn assert_send_sync_static<T: Send + Sync + 'static>() {}
        assert_send_sync_static::<Error>();
    }
}
```

### 4.2 Adapter: same source type in two places

Writing `#[from]` twice generates conflicting `From<io::Error>` impls and erases
whether the failure happened on read or write. Write the fields explicitly and use
`#[source]`.

```rust
use std::io;
use std::path::PathBuf;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    #[error("could not read `{path}`")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not write `{path}`")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("`{path}` is not valid JSON")]
    Decode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

impl Error {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } => matches!(
                source.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ),
            Self::Decode { .. } => false,
        }
    }
}
```

### 4.3 Service: wrap inner errors and add classification

```rust
use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    #[error("{entity} {id} was not found")]
    NotFound { entity: &'static str, id: u64 },

    /// Delegate Display to the inner error.
    #[error(transparent)]
    Domain(#[from] domain::Error),

    /// Delegate Display to the inner error.
    #[error(transparent)]
    Store(#[from] store::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    InvalidInput,
    NotFound,
    Storage,
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::NotFound { .. } => ErrorKind::NotFound,
            Self::Domain(_) => ErrorKind::InvalidInput,
            Self::Store(_) => ErrorKind::Storage,
        }
    }

    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Store(err) => err.is_retryable(),
            Self::NotFound { .. } | Self::Domain(_) => false,
        }
    }
}
```

### 4.4 Binary: context + downcast + exit codes

`Service`, `from_path`, and `run` are placeholders; replace them with your API.

```rust
use std::process::ExitCode;

use anyhow::Context;
use service::{ErrorKind, Service};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Print context and every source once.
            eprintln!("error: {err:?}");
            ExitCode::from(exit_code(&err))
        }
    }
}

fn run() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("usage: app <CONFIG_PATH>")?;

    let service = Service::from_path(&path)
        .with_context(|| format!("could not initialize service with `{path}`"))?;

    service
        .run()
        .with_context(|| format!("could not process `{path}`"))?;

    Ok(())
}

/// `anyhow::Error::downcast_ref` sees through `.context()` layers.
fn exit_code(err: &anyhow::Error) -> u8 {
    let Some(err) = err.downcast_ref::<service::Error>() else {
        return 1;
    };

    match err.kind() {
        ErrorKind::InvalidInput => 65,                  // EX_DATAERR
        ErrorKind::NotFound => 66,                      // EX_NOINPUT
        ErrorKind::Storage if err.is_retryable() => 75, // EX_TEMPFAIL
        ErrorKind::Storage => 74,                       // EX_IOERR
        _ => 1,
    }
}
```

For stricter separation, parse argv first, handle a dedicated `UsageError`, print
usage, and return exit code 2. Keep that path out of the `anyhow` chain.

### 4.5 Opaque error hiding the public representation

When internal variants may change, expose an opaque struct plus `kind()`.

```rust
use thiserror::Error;

#[derive(Debug, Error)]
#[error(transparent)]
pub struct PublicError(#[from] ErrorRepr);

impl PublicError {
    /// Construct a `NotFound` error.
    pub fn not_found() -> Self {
        Self(ErrorRepr::NotFound)
    }

    pub fn kind(&self) -> ErrorKind {
        match &self.0 {
            ErrorRepr::InvalidInput(_) => ErrorKind::InvalidInput,
            ErrorRepr::NotFound => ErrorKind::NotFound,
            ErrorRepr::Storage(_) => ErrorKind::Storage,
        }
    }
}

impl From<domain::Error> for PublicError {
    fn from(err: domain::Error) -> Self {
        Self(ErrorRepr::InvalidInput(err))
    }
}

impl From<store::Error> for PublicError {
    fn from(err: store::Error) -> Self {
        Self(ErrorRepr::Storage(err))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    InvalidInput,
    NotFound,
    Storage,
}

/// Private. Free to change.
#[derive(Debug, Error)]
enum ErrorRepr {
    #[error(transparent)]
    InvalidInput(domain::Error),

    #[error("requested item was not found")]
    NotFound,

    #[error(transparent)]
    Storage(store::Error),
}
```

### 4.6 Async boundaries

- Errors crossing threads or `tokio::spawn` must be `Send + Sync + 'static`.
- Classify `JoinError` (panic/cancellation) separately from domain failures.
- Cancellation is not a retryable failure; give it its own kind.
- Do not hold non-`Send` values across `.await` points.

---

## 5. `Display` and source-chain rules

### 5.1 Basics

- `Display`: one sentence describing **what failed at this layer**. Lowercase, no
  trailing period.
- `source()`: the wrapped lower-level error, marked with `#[source]`, `#[from]`, or
  a field named `source`.
- `Debug`: for logs and debugging. Never use it as a user-facing message.

```rust
// Good: message and cause are separate.
#[error("could not read config `{path}`")]
Read {
    path: PathBuf,
    #[source]
    source: io::Error,
}
```

```rust
// Bad: the source also appears in Display, so reporters print it twice.
#[error("could not read config: {source}")]
Read {
    path: PathBuf,
    #[source]
    source: io::Error,
}
```

Example output:

```text
error: could not start app

Caused by:
    0: could not read config `config.toml`
    1: No such file or directory (os error 2)
```

### 5.2 What `#[error(transparent)]` means

- It **delegates** `Display` and `source()` to the inner error.
- The wrapper does not appear in the source chain, and no intermediate message is
  added.
- The inner error is still matchable through the variant.
- If the chain needs a layer marker, use a distinct message plus `#[source]`
  instead of transparent, but do not repeat the inner message in `Display`.

```rust
// Delegation: no intermediate message.
#[error(transparent)]
Store(#[from] store::Error),

// Layer marker: the cause stays in source().
#[error("storage operation failed")]
Store(#[from] store::Error),
```

### 5.3 `anyhow` output

```rust
eprintln!("error: {err:?}"); // Multi-line, includes Caused by.
eprintln!("error: {err:#}"); // One line: context: cause: cause.
```

`anyhow` captures and prints a backtrace when `RUST_BACKTRACE=1` is set. A
`Backtrace` field in `thiserror` may require nightly depending on the toolchain
version, so check the MSRV.

### 5.4 Writing messages

- Good: `task 42 was not found`, `could not read task file /data/tasks.json`
- Bad: `Error`, `Something went wrong`, `failed`
- Include **investigation values** in the context: path, id, key, request id.
- Do not expose internal implementation terms (for example, "mutex poisoned") in a
  user-facing message.
- Do not use error messages as dashboard or alert keys; they change. Use `kind` or
  an explicit error code instead.

---

## 6. Workspace configuration template

### 6.1 Root `Cargo.toml`

```toml
[workspace]
resolver = "3" # Default for edition 2024. Use "2" below Rust 1.84.
members = ["crates/*"]

[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
license = "MIT OR Apache-2.0"

[workspace.dependencies]
thiserror = "2"
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[workspace.lints.rust]
unsafe_code = "forbid"

# Keep restriction lints as warnings and promote them in CI with -D warnings.
[workspace.lints.clippy]
unwrap_used = "warn"
expect_used = "warn"
panic = "warn"
todo = "warn"
```

### 6.2 Member crate

```toml
[package]
name = "domain"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
thiserror.workspace = true
serde.workspace = true

[lints]
workspace = true
```

```rust
// Allow unwrap only in tests.
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
}
```

### 6.3 Bootstrapping a new workspace

```bash
cargo new --lib crates/domain
cargo new --lib crates/store
cargo new --lib crates/service
cargo new --bin crates/app
```

Then wire path dependencies according to the crate graph
(`app → service → {domain, store} → domain`) and place the templates above.

### 6.4 Public API hygiene

- A public error type must implement
  `Debug + Display + std::error::Error + Send + Sync + 'static`.
- Put `#[non_exhaustive]` on public enums.
- The `From` impl generated by `#[from]` is part of the public API. Add it only at
  boundaries that actually use `?`.
- A variant field such as `io::Error` or `serde_json::Error` becomes part of the
  public API too. Weigh matchability against dependency coupling; if the coupling
  is too costly, hide it behind an opaque struct plus `kind()`.
- Prefer an opaque type when the internal representation is likely to change.
- Document `# Errors` and each variant's meaning in rustdoc.
- If the API is already published, follow semver rules for adding or removing
  variants.

---

## 7. Exit codes and retries

### 7.1 Exit codes

Using `sysexits.h` conventions helps shells and operations tooling distinguish
failure causes.

| Code | Name | Meaning |
| --- | --- | --- |
| 0 | success | Success |
| 1 | failure | Unclassified general failure |
| 2 | usage | Invalid command or arguments |
| 65 | `EX_DATAERR` | Input/domain validation failure |
| 66 | `EX_NOINPUT` | Requested entity does not exist |
| 74 | `EX_IOERR` | I/O or parsing failure |
| 75 | `EX_TEMPFAIL` | Transient failure that may be retryable |

- POSIX shells only preserve 0-255. Windows uses 32-bit exit values.
- If a team or platform convention exists, prefer it.
- Return `ExitCode` explicitly on the success path too.

### 7.2 Retries

- The error type provides only a **hint** such as `is_retryable()`.
- Retry policy belongs to the service or caller: maximum attempts, backoff, jitter,
  and budget.
- Never automatically retry non-idempotent writes, payments, or message publishes.
- Cancellation, authentication failures, and validation failures are not retryable.
- Separate retryable failures from non-retryable ones with `kind()` and a dedicated
  flag.

---

## 8. Testing strategy

Errors are contracts. Test messages, variants, source chains, classification, and
exit codes.

### 8.1 Unit tests

```rust
#[test]
fn variant_and_display_are_stable() {
    let err = User::new("  ").unwrap_err();
    assert!(matches!(err, Error::BlankName));
    assert_eq!(err.to_string(), "name must not be blank");
}

#[test]
fn source_chain_is_preserved() {
    use std::error::Error as _;

    let err = load_from_broken_file();
    assert!(err.source().is_some(), "inner error must be reachable");
}

#[test]
fn classification_is_stable() {
    let err = service_call_that_is_missing();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert!(!err.is_retryable());
}

#[test]
fn error_is_send_sync_static() {
    fn assert_send_sync_static<T: Send + Sync + 'static>() {}
    assert_send_sync_static::<Error>();
}
```

### 8.2 CLI integration tests

Run the real binary and verify the exit code and stderr.

```rust
use std::process::Command;

#[test]
fn missing_entity_uses_ex_noinput() {
    let output = Command::new(env!("CARGO_BIN_EXE_app"))
        .args(["get", "42"])
        .env("RUST_BACKTRACE", "0")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(66));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("was not found"), "{stderr}");
}
```

> In `CARGO_BIN_EXE_<name>`, `<name>` is the binary name with hyphens replaced by
> underscores.

### 8.3 Duplicate-output and chain tests

```rust
assert_eq!(stderr.matches("task title must not be blank").count(), 1, "{stderr}");
assert!(stderr.contains("Caused by:"), "{stderr}");
```

### 8.4 Corrupt data

- Invalid JSON, binary data, or encoding
- Well-formed data that violates domain rules
- Empty files, missing files, permission errors
- Boundary values (empty strings, maximum length, zero, negative numbers, overflow)

### 8.5 Snapshots

Tools such as `insta` are fine, but error messages are **user interfaces**. Always
review snapshot changes.

---

## 9. Anti-patterns and fixes

| Anti-pattern | Problem | Fix |
| --- | --- | --- |
| `Result<T, String>` | No matching, no source, lost context | crate-local `Error` enum |
| Public API returns `anyhow::Result` | Consumers cannot distinguish failures | `thiserror` typed error |
| Public API returns `Box<dyn Error>` | Not `Send + Sync`, downcast-dependent | Typed error, or `Box<dyn Error + Send + Sync + 'static>` |
| Shared `errors` crate | Coupling, cycles, wider rebuilds | Per-boundary errors plus wrapping |
| `map_err(|e| Other(e.to_string()))` | Breaks the source chain | Preserve the source with `#[source]`/`.context()` |
| `#[error("...: {source}")]` + `#[source]` | Duplicate cause output | Remove the source from the message or use transparent |
| Logging and returning in one layer | Duplicate logs | Log once at the top level |
| `unwrap()` on user input | Panics, ignores input errors | `?`, `let Ok(..) else`, `unwrap_or_else` |
| Every failure becomes `Other` | Callers cannot branch | Add variants only for failures handled differently |
| Using `Option` for a failure | Loses the reason and id | `Result` + `NotFound` |
| Huge enum with no `kind` | Consumers couple to every variant | Expose a small `ErrorKind` |
| Always exiting with 1 | Operators cannot distinguish causes | Map `kind` to exit codes |
| Foreign error types in public variants | Dependency/version coupling | Consider an opaque type + `kind` |

### Stringified context

```rust
// Bad
return Err(Error::Other(format!("could not read {path}: {e}")));

// Good
return Err(Error::Read { path, source: e });
```

### Log and return

```rust
// Bad
match do_work() {
    Err(e) => {
        tracing::error!("work failed: {e}");
        return Err(e);
    }
    Ok(v) => v,
}

// Good: report once at the top level with the chain.
let value = do_work().context("work failed")?;
```

---

## 10. Review checklist

- [ ] Does every public fallible function return a crate-owned error type?
- [ ] Are `anyhow::Error`, `Box<dyn Error>`, and `String` absent from public APIs?
- [ ] Does the error type implement
      `Debug + Display + Error + Send + Sync + 'static`?
- [ ] Does `Display` start lowercase and avoid a trailing period?
- [ ] Do `Display` and `source()` avoid printing the same cause twice?
- [ ] Is the cause chain preserved with `#[source]`/`#[from]`?
- [ ] Is the same source type absent from multiple variants? (If present, use
      explicit variants.)
- [ ] Do public enums have `#[non_exhaustive]`?
- [ ] Are `kind()`/`is_retryable()` present where consumers need classification?
- [ ] Do `From` impls and context addition happen only at real boundaries?
- [ ] Does the binary report an error once and map it to an exit code?
- [ ] Are `panic`/`unwrap` limited to tests and proven invariants?
- [ ] Do tests cover variants, source chains, exit codes, and corrupt data?
- [ ] Does rustdoc document `# Errors` and the meaning of each variant?
- [ ] Do logs include structured fields (path, id, request id)?

---

## 11. Migration playbook

Migrate incrementally without breaking public APIs.

1. **Inventory**: list `Result<_, String>`, public `Box<dyn Error>`/`anyhow` usage,
   `unwrap`/`expect`, and duplicate logging.
2. **Boundary table**: record every public function and its consumers per crate.
3. **Add the new type**: introduce a crate-local `Error` + `Result` while keeping
   the old type.
4. **Migrate from the inside out**: domain → adapter → service, one boundary at a
   time.
5. **Migrate the binary**: adopt `anyhow` + context + downcast + exit codes.
6. **Strengthen tests**: add tests for new variants, sources, and exit codes.
7. **Deprecate**: mark the old public error type/function deprecated and remove it
   at the next major version.
8. **CI gates**: make clippy `-D warnings`, fmt, doc, and tests mandatory.

---

## 12. Alternative tools

| Tool/pattern | Best for | Notes |
| --- | --- | --- |
| `thiserror` + `anyhow` | Most workspaces | Default choice |
| `snafu` | Multiple contexts for the same source type, context selectors | Large workspaces |
| `miette` | Rich CLI diagnostics with spans, codes, and hints | `Diagnostic` derive |
| `eyre`/`color-eyre` | Application reporting (backtraces/color) | `anyhow` alternative |
| Opaque struct + `kind()` | Hiding internals while keeping stable classification | `std::io::Error` style |
| RFC 9457 Problem Details | HTTP API error responses | Map `kind()` to status/type/title |

Regardless of the tool:

> **Meaning belongs in the library, policy belongs to the consumer, and
> presentation belongs at the top level.**

---

## 13. References

- Rust API Guidelines — C-GOOD-ERR:
  <https://rust-lang.github.io/api-guidelines/interoperability.html#c-good-err>
- `std::error::Error`:
  <https://doc.rust-lang.org/std/error/trait.Error.html>
- `thiserror`:
  <https://docs.rs/thiserror>
- `anyhow`:
  <https://docs.rs/anyhow>
- `snafu`:
  <https://docs.rs/snafu>
- `miette`:
  <https://docs.rs/miette>
- `sysexits.h`:
  <https://man.openbsd.org/sysexits>
- RFC 9457 Problem Details:
  <https://www.rfc-editor.org/rfc/rfc9457>
- Rust Error Handling (Microsoft RustTraining, ch.10):
  <https://github.com/microsoft/RustTraining/blob/main/rust-patterns-book/src/ch10-error-handling-patterns.md>
- Error Handling for Large Rust Projects (GreptimeDB):
  <https://greptime.com/blogs/2024-05-07-error-rust>
- Rust Best Practices (Apollo, ch.4):
  <https://github.com/apollographql/rust-best-practices/blob/main/book/chapter_04.md>
