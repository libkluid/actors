//! Single-use reply handles.

use std::fmt;

use crossbeam_channel::{Receiver, Sender, bounded};
use thiserror::Error;

/// The caller stopped waiting before the actor sent its reply.
///
/// This is normal when a request times out: the actor keeps running and its
/// handler can ignore the failed send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("caller is no longer waiting for a reply")]
pub struct ReplyDropped;

/// A single-use handle that a handler uses to answer an [`ask`] request.
///
/// The handle travels inside the actor's message, so replying is type-checked
/// against the request. Dropping it without calling [`send`](Self::send) makes
/// the caller observe [`Error::NoReply`](crate::Error::NoReply).
///
/// [`ask`]: crate::Address::ask
pub struct Reply<T> {
    sender: Sender<T>,
}

impl<T> Reply<T> {
    pub(crate) fn channel() -> (Self, Receiver<T>) {
        let (sender, receiver) = bounded(1);
        (Self { sender }, receiver)
    }

    /// Sends the value to the waiting caller.
    ///
    /// # Errors
    ///
    /// Returns [`ReplyDropped`] when the caller is no longer waiting, for
    /// example because [`Address::ask_timeout`](crate::Address::ask_timeout)
    /// expired. Handlers may ignore this result; it does not indicate an actor
    /// failure.
    pub fn send(self, value: T) -> Result<(), ReplyDropped> {
        self.sender.send(value).map_err(|_| ReplyDropped)
    }
}

impl<T> fmt::Debug for Reply<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Reply<_>")
    }
}
