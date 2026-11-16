//! A counter actor that demonstrates domain errors inside handlers.
//!
//! The actor owns a single `u64`. Fire-and-forget increments are best-effort
//! and saturate at [`u64::MAX`]; callers that need to detect overflow use
//! [`Message::AddChecked`], whose reply carries `Result<u64, Error>`.
//!
//! The error type belongs to this crate. The actor runtime never inspects it,
//! and the binary maps it to an exit code only at the top level.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::num::NonZeroUsize;

use actor::{Actor, ActorRef, Context, Reply};
use thiserror::Error;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// A failure produced by a counter operation.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// Adding `amount` to `current` would exceed `max`.
    #[error("counter overflow: {current} + {amount} exceeds {max}")]
    Overflow {
        /// Value before the rejected increment.
        current: u64,
        /// Increment that was rejected.
        amount: u64,
        /// Largest representable value.
        max: u64,
    },
}

/// Commands accepted by [`Counter`].
#[derive(Debug)]
#[non_exhaustive]
pub enum Message {
    /// Best-effort increment. Saturates instead of reporting overflow.
    Add(u64),

    /// Increment whose result is returned to the caller.
    AddChecked {
        /// Amount to add.
        amount: u64,
        /// Reply carrying the new value or [`Error::Overflow`].
        reply: Reply<Result<u64>>,
    },

    /// Reads the current value.
    Value(Reply<u64>),
}

/// An actor that owns a `u64` and serializes every update.
#[derive(Debug, Default)]
pub struct Counter {
    value: u64,
}

impl Counter {
    /// Creates a counter at zero.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Actor for Counter {
    type Message = Message;

    fn name(&self) -> &'static str {
        "counter"
    }

    fn handle(&mut self, message: Self::Message, _ctx: &mut Context<Self>) {
        match message {
            Message::Add(amount) => {
                self.value = self.value.saturating_add(amount);
            }
            Message::AddChecked { amount, reply } => {
                let outcome = match self.value.checked_add(amount) {
                    Some(next) => {
                        self.value = next;
                        Ok(next)
                    }
                    None => Err(Error::Overflow {
                        current: self.value,
                        amount,
                        max: u64::MAX,
                    }),
                };
                let _ = reply.send(outcome);
            }
            Message::Value(reply) => {
                let _ = reply.send(self.value);
            }
        }
    }
}

/// Starts a counter actor with a bounded mailbox.
///
/// # Errors
///
/// Returns [`actor::Error::Spawn`] if the actor thread cannot be created.
pub fn spawn(capacity: NonZeroUsize) -> actor::Result<ActorRef<Counter>> {
    actor::spawn_bounded(Counter::new(), capacity)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::num::NonZeroUsize;

    use super::*;

    fn capacity(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("test capacity is non-zero")
    }

    #[test]
    fn increments_are_serialized_and_readable() {
        let counter = spawn(capacity(32)).expect("counter starts");

        for _ in 0..1_000 {
            counter.tell(Message::Add(1)).expect("increment is queued");
        }
        let value = counter.ask(Message::Value).expect("value is returned");

        assert_eq!(value, 1_000);
        counter.shutdown().expect("counter stops");
    }

    #[test]
    fn checked_increment_reports_overflow_without_killing_the_actor() {
        let counter = spawn(capacity(8)).expect("counter starts");

        let seeded = counter
            .ask(|reply| Message::AddChecked { amount: 2, reply })
            .expect("checked increment is answered");
        assert!(matches!(seeded, Ok(2)));

        let overflow = counter
            .ask(|reply| Message::AddChecked {
                amount: u64::MAX,
                reply,
            })
            .expect("checked increment is answered");
        assert!(matches!(
            overflow,
            Err(Error::Overflow {
                current: 2,
                amount: u64::MAX,
                max: u64::MAX
            })
        ));

        // A rejected operation is data, not a crash: the actor is still usable.
        let value = counter.ask(Message::Value).expect("value is returned");
        assert_eq!(value, 2);

        counter.shutdown().expect("counter stops");
    }

    #[test]
    fn error_display_is_stable() {
        let error = Error::Overflow {
            current: 1,
            amount: 2,
            max: 2,
        };
        assert_eq!(error.to_string(), "counter overflow: 1 + 2 exceeds 2");
    }

    #[test]
    fn default_name_is_short_and_explicit() {
        assert_eq!(Counter::new().name(), "counter");
    }

    #[test]
    fn error_is_send_sync_static() {
        fn assert_send_sync_static<T: Send + Sync + 'static>() {}
        assert_send_sync_static::<Error>();
    }
}
