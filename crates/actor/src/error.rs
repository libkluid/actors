//! Typed errors produced by the actor runtime.

use std::fmt;
use std::io;
use std::time::Duration;

use thiserror::Error;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// A mailbox or lifecycle failure.
///
/// This type deliberately does **not** describe domain failures. A handler that
/// rejects work should answer with `Reply<Result<T, E>>`, where `E` is owned by
/// the actor's crate. Keeping the two concerns apart means:
///
/// - callers can retry transport failures without retrying rejected work, and
/// - actor crates can evolve their domain errors without touching this enum.
///
/// The enum is `#[non_exhaustive]`: downstream matches need a wildcard arm.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// The actor thread has exited and its mailbox is closed.
    #[error("actor `{actor}` is not running")]
    Stopped {
        /// Name reported by [`Actor::name`](crate::Actor::name).
        actor: &'static str,
    },

    /// A send or a reply did not complete before the caller's deadline.
    #[error("actor `{actor}` did not {operation} within {timeout:?}")]
    Timeout {
        /// Actor involved in the request.
        actor: &'static str,
        /// Step that exceeded the deadline.
        operation: Operation,
        /// Deadline supplied by the caller.
        timeout: Duration,
    },

    /// The actor accepted the request but dropped the reply handle.
    ///
    /// This is usually a handler bug: every request message owns a [`Reply`]
    /// and must eventually call [`Reply::send`] or consciously drop it.
    ///
    /// [`Reply`]: crate::Reply
    /// [`Reply::send`]: crate::Reply::send
    #[error("actor `{actor}` dropped the reply without answering")]
    NoReply {
        /// Actor that handled the message.
        actor: &'static str,
    },

    /// A non-blocking send found every slot of a bounded mailbox occupied.
    #[error("mailbox for actor `{actor}` is full")]
    MailboxFull {
        /// Actor whose mailbox rejected the message.
        actor: &'static str,
    },

    /// The actor thread unwound, either in a handler or in a lifecycle hook.
    #[error("actor `{actor}` panicked: {panic}")]
    Panicked {
        /// Actor that panicked.
        actor: &'static str,
        /// Panic payload converted to text.
        panic: String,
    },

    /// The operating system refused to create the actor thread.
    #[error("could not start actor `{actor}`")]
    Spawn {
        /// Actor that could not start.
        actor: &'static str,
        /// Underlying thread creation failure.
        #[source]
        source: io::Error,
    },
}

/// The step of a request that exceeded its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Operation {
    /// Waiting for space in a bounded mailbox.
    Send,
    /// Waiting for the handler to answer.
    Reply,
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Send => f.write_str("accept a message"),
            Self::Reply => f.write_str("send a reply"),
        }
    }
}

/// Coarse classification for consumers that should not match every variant.
///
/// `kind()` is the stable part of the API. New variants may be added behind
/// this enum without forcing every consumer to change its policy mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The actor is no longer running.
    Stopped,
    /// A deadline expired.
    Timeout,
    /// A request was accepted but never answered.
    NoReply,
    /// A bounded mailbox is full.
    Overloaded,
    /// The actor thread panicked.
    Panicked,
    /// The actor thread could not be created.
    Spawn,
}

impl Error {
    /// Returns the stable kind of this error.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::Stopped { .. } => ErrorKind::Stopped,
            Self::Timeout { .. } => ErrorKind::Timeout,
            Self::NoReply { .. } => ErrorKind::NoReply,
            Self::MailboxFull { .. } => ErrorKind::Overloaded,
            Self::Panicked { .. } => ErrorKind::Panicked,
            Self::Spawn { .. } => ErrorKind::Spawn,
        }
    }

    /// Returns `true` when retrying the whole request may succeed.
    ///
    /// This is only a hint. Timeouts and overload are transient, but automatic
    /// retries are still the caller's decision: retrying a non-idempotent
    /// message can duplicate work. `NoReply`, `Panicked`, `Stopped`, and
    /// `Spawn` are not treated as retryable.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Timeout { .. } | Self::MailboxFull { .. })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::error::Error as _;

    use super::*;

    #[test]
    fn display_messages_are_stable() {
        let stopped = Error::Stopped { actor: "counter" };
        assert_eq!(stopped.to_string(), "actor `counter` is not running");

        let timeout = Error::Timeout {
            actor: "counter",
            operation: Operation::Reply,
            timeout: Duration::from_millis(50),
        };
        assert_eq!(
            timeout.to_string(),
            "actor `counter` did not send a reply within 50ms"
        );

        let full = Error::MailboxFull { actor: "counter" };
        assert_eq!(full.to_string(), "mailbox for actor `counter` is full");
    }

    #[test]
    fn kind_mapping_and_retryability_are_stable() {
        let cases = [
            (Error::Stopped { actor: "a" }, ErrorKind::Stopped, false),
            (
                Error::Timeout {
                    actor: "a",
                    operation: Operation::Send,
                    timeout: Duration::from_secs(1),
                },
                ErrorKind::Timeout,
                true,
            ),
            (Error::NoReply { actor: "a" }, ErrorKind::NoReply, false),
            (
                Error::MailboxFull { actor: "a" },
                ErrorKind::Overloaded,
                true,
            ),
            (
                Error::Panicked {
                    actor: "a",
                    panic: "boom".to_owned(),
                },
                ErrorKind::Panicked,
                false,
            ),
        ];

        for (error, kind, retryable) in cases {
            assert_eq!(error.kind(), kind, "{error}");
            assert_eq!(error.is_retryable(), retryable, "{error}");
        }
    }

    #[test]
    fn spawn_preserves_the_source() {
        let error = Error::Spawn {
            actor: "a",
            source: io::Error::other("no threads left"),
        };

        assert_eq!(error.kind(), ErrorKind::Spawn);
        assert!(!error.is_retryable());
        assert_eq!(
            error
                .source()
                .expect("spawn error has a source")
                .to_string(),
            "no threads left"
        );
    }

    #[test]
    fn error_is_send_sync_static() {
        fn assert_send_sync_static<T: Send + Sync + 'static>() {}
        assert_send_sync_static::<Error>();
    }
}
