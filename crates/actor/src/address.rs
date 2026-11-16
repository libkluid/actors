//! Cloneable actor addresses and the send/reply protocol.

use std::fmt;
use std::time::{Duration, Instant};

use crossbeam_channel::{RecvTimeoutError, SendTimeoutError, Sender, TrySendError};

use crate::error::{Error, Operation, Result};
use crate::reply::Reply;
use crate::runtime::{Actor, Envelope};

/// A cloneable handle to an actor's mailbox.
///
/// Cloning is cheap: every clone is another sender for the same channel.
/// Addresses are `Send + Sync`, so they can be moved into worker threads.
pub struct Address<A: Actor> {
    actor: &'static str,
    sender: Sender<Envelope<A::Message>>,
}

impl<A: Actor> Address<A> {
    pub(crate) fn new(actor: &'static str, sender: Sender<Envelope<A::Message>>) -> Self {
        Self { actor, sender }
    }

    /// Returns the actor name used in errors.
    pub fn actor_name(&self) -> &'static str {
        self.actor
    }

    /// Returns the mailbox capacity, or `None` for an unbounded mailbox.
    pub fn capacity(&self) -> Option<usize> {
        self.sender.capacity()
    }

    /// Returns the number of messages currently queued.
    ///
    /// The value is approximate under concurrent sends; use it for
    /// observability, not as a synchronization primitive.
    pub fn queued(&self) -> usize {
        self.sender.len()
    }

    /// Sends a message, waiting for mailbox space when necessary.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Stopped`] after the actor thread has exited.
    pub fn tell(&self, message: A::Message) -> Result<()> {
        self.sender
            .send(Envelope::Message(message))
            .map_err(|_| Error::Stopped { actor: self.actor })
    }

    /// Tries to send without waiting.
    ///
    /// # Errors
    ///
    /// - [`Error::MailboxFull`] when a bounded mailbox has no free slot.
    /// - [`Error::Stopped`] after the actor thread has exited.
    pub fn try_tell(&self, message: A::Message) -> Result<()> {
        self.sender
            .try_send(Envelope::Message(message))
            .map_err(|error| match error {
                TrySendError::Full(_) => Error::MailboxFull { actor: self.actor },
                TrySendError::Disconnected(_) => Error::Stopped { actor: self.actor },
            })
    }

    /// Sends a message, waiting at most `timeout` for mailbox space.
    ///
    /// # Errors
    ///
    /// - [`Error::Timeout`] with [`Operation::Send`] when the mailbox stays full.
    /// - [`Error::Stopped`] after the actor thread has exited.
    pub fn tell_timeout(&self, message: A::Message, timeout: Duration) -> Result<()> {
        self.sender
            .send_timeout(Envelope::Message(message), timeout)
            .map_err(|error| match error {
                SendTimeoutError::Timeout(_) => Error::Timeout {
                    actor: self.actor,
                    operation: Operation::Send,
                    timeout,
                },
                SendTimeoutError::Disconnected(_) => Error::Stopped { actor: self.actor },
            })
    }

    /// Sends a request and blocks until the handler replies.
    ///
    /// `build` receives the reply handle and returns the request message. The
    /// type of the reply is inferred from the message field, so mismatches do
    /// not compile.
    ///
    /// This method has no deadline. Use [`ask_timeout`](Self::ask_timeout) when
    /// a slow or wedged actor must not block the caller forever. Never call
    /// `ask` from a handler back into the same actor: the handler owns the only
    /// thread that could answer.
    ///
    /// # Errors
    ///
    /// - [`Error::Stopped`] when the request cannot be delivered.
    /// - [`Error::NoReply`] when the actor exits or drops the reply handle.
    pub fn ask<T, F>(&self, build: F) -> Result<T>
    where
        T: Send,
        F: FnOnce(Reply<T>) -> A::Message,
    {
        let (reply, receiver) = Reply::channel();
        self.sender
            .send(Envelope::Message(build(reply)))
            .map_err(|_| Error::Stopped { actor: self.actor })?;

        receiver
            .recv()
            .map_err(|_| Error::NoReply { actor: self.actor })
    }

    /// Sends a request and waits at most `timeout` for both mailbox space and
    /// the reply.
    ///
    /// The deadline covers the whole request, not each step independently.
    ///
    /// # Errors
    ///
    /// - [`Error::Timeout`] with [`Operation::Send`] when the mailbox stays full.
    /// - [`Error::Timeout`] with [`Operation::Reply`] when the handler is too slow.
    /// - [`Error::Stopped`] when the request cannot be delivered.
    /// - [`Error::NoReply`] when the actor exits or drops the reply handle.
    pub fn ask_timeout<T, F>(&self, timeout: Duration, build: F) -> Result<T>
    where
        T: Send,
        F: FnOnce(Reply<T>) -> A::Message,
    {
        let start = Instant::now();
        let (reply, receiver) = Reply::channel();

        self.sender
            .send_timeout(Envelope::Message(build(reply)), timeout)
            .map_err(|error| match error {
                SendTimeoutError::Timeout(_) => Error::Timeout {
                    actor: self.actor,
                    operation: Operation::Send,
                    timeout,
                },
                SendTimeoutError::Disconnected(_) => Error::Stopped { actor: self.actor },
            })?;

        let remaining = timeout.saturating_sub(start.elapsed());
        receiver
            .recv_timeout(remaining)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => Error::Timeout {
                    actor: self.actor,
                    operation: Operation::Reply,
                    timeout,
                },
                RecvTimeoutError::Disconnected => Error::NoReply { actor: self.actor },
            })
    }

    /// Queues a graceful stop after already-sent messages.
    ///
    /// Sending blocks when the mailbox is full. Use [`ActorRef::shutdown`] to
    /// wait for the thread as well.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Stopped`] when the actor has already exited.
    ///
    /// [`ActorRef::shutdown`]: crate::ActorRef::shutdown
    pub fn stop(&self) -> Result<()> {
        self.sender
            .send(Envelope::Stop)
            .map_err(|_| Error::Stopped { actor: self.actor })
    }
}

impl<A: Actor> Clone for Address<A> {
    fn clone(&self) -> Self {
        Self {
            actor: self.actor,
            sender: self.sender.clone(),
        }
    }
}

impl<A: Actor> fmt::Debug for Address<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Address")
            .field("actor", &self.actor)
            .field("capacity", &self.capacity())
            .field("queued", &self.queued())
            .finish()
    }
}
