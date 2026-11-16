//! Actor definitions, spawning, and the per-actor thread loop.

use std::any::{Any, type_name};
use std::fmt;
use std::marker::PhantomData;
use std::num::NonZeroUsize;
use std::ops::Deref;
use std::thread;

use crossbeam_channel::{Receiver, unbounded};

use crate::address::Address;
use crate::error::{Error, Result};

/// Mailbox capacity used by [`spawn`].
///
/// A bounded mailbox is the safe default: producers block instead of growing
/// memory without limit when the actor cannot keep up.
pub const DEFAULT_MAILBOX_CAPACITY: usize = 1024;

/// Internal envelope: a user message or a request to stop after prior work.
pub(crate) enum Envelope<M> {
    Message(M),
    Stop,
}

/// A unit of isolated mutable state driven by messages.
///
/// The actor lives on its own thread. Only [`handle`](Self::handle) can touch
/// `&mut self`, so no lock is required around actor state.
///
/// # Message ordering
///
/// Messages from one [`Address`] are handled in send order. Messages from
/// different addresses interleave; the runtime does not promise a global order.
pub trait Actor: Sized + Send + 'static {
    /// The message type accepted by this actor.
    ///
    /// Use an enum when the actor accepts several commands. Requests that need
    /// an answer carry a [`Reply<T>`](crate::Reply) field.
    type Message: Send + 'static;

    /// Name used in errors, logs, and the OS thread name.
    ///
    /// The default is the short Rust type name. Override it when the type name
    /// is not a stable or readable identifier.
    fn name(&self) -> &'static str {
        short_type_name::<Self>()
    }

    /// Called once on the actor thread before the first message.
    ///
    /// Calling [`Context::stop`] here skips the message loop but still runs
    /// [`stopped`](Self::stopped).
    fn started(&mut self, _ctx: &mut Context<Self>) {}

    /// Handles one message.
    ///
    /// [`Reply::send`](crate::Reply::send) may fail when the caller has timed
    /// out; that is not a reason to panic or stop. Use [`Context::stop`] to
    /// exit after this message.
    ///
    /// A panic here terminates the actor thread and is reported by
    /// [`JoinHandle::join`] as [`Error::Panicked`].
    fn handle(&mut self, message: Self::Message, ctx: &mut Context<Self>);

    /// Called on the actor thread after the loop ends, including after
    /// [`Context::stop`] and a normal mailbox disconnect.
    ///
    /// It is not called when [`handle`](Self::handle) panics.
    fn stopped(&mut self, _ctx: &mut Context<Self>) {}
}

/// Per-message runtime control available to an actor.
///
/// The context is intentionally small: stopping is the only lifecycle decision
/// a handler can make. Supervision, restarts, and discovery belong to a higher
/// layer.
pub struct Context<A: Actor> {
    actor: &'static str,
    stopping: bool,
    marker: PhantomData<fn() -> A>,
}

impl<A: Actor> Context<A> {
    pub(crate) fn new(actor: &'static str) -> Self {
        Self {
            actor,
            stopping: false,
            marker: PhantomData,
        }
    }

    /// Returns the actor name.
    pub fn actor_name(&self) -> &'static str {
        self.actor
    }

    /// Requests a graceful stop after the current message.
    ///
    /// Messages already queued behind the current one are dropped. Use
    /// [`ActorRef::shutdown`] or [`Address::stop`] for a drain-first shutdown.
    pub fn stop(&mut self) {
        self.stopping = true;
    }

    /// Returns `true` once [`stop`](Self::stop) has been called.
    pub fn is_stopping(&self) -> bool {
        self.stopping
    }
}

impl<A: Actor> fmt::Debug for Context<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("actor", &self.actor)
            .field("stopping", &self.stopping)
            .finish()
    }
}

/// A running actor together with the handle for its thread.
///
/// `ActorRef` dereferences to [`Address`], so `tell`/`ask`/`stop` are available
/// directly. Dropping the last `Address` closes the mailbox and lets the actor
/// drain queued messages and exit; dropping the `JoinHandle` detaches the
/// thread instead of waiting for it.
pub struct ActorRef<A: Actor> {
    address: Address<A>,
    join: JoinHandle,
}

impl<A: Actor> ActorRef<A> {
    /// Returns a reference to the actor's address.
    pub fn address(&self) -> &Address<A> {
        &self.address
    }

    /// Splits the reference into a cloneable address and a join handle.
    ///
    /// This is useful when the caller wants to drop every address and then join
    /// the thread, or wants to keep sending from several threads.
    pub fn into_parts(self) -> (Address<A>, JoinHandle) {
        (self.address, self.join)
    }

    /// Stops the actor and waits for its thread.
    ///
    /// A stop request is queued behind messages that were already sent, so the
    /// actor drains them first. If the actor has already stopped or panicked,
    /// the join result is still returned.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Panicked`] when the actor thread panicked, or another
    /// [`Error`] if the stop request could not be delivered. An actor that has
    /// already stopped is treated as successfully shut down.
    pub fn shutdown(self) -> Result<()> {
        match self.address.stop() {
            Ok(()) | Err(Error::Stopped { .. }) => {}
            Err(error) => return Err(error),
        }

        self.join.join()
    }
}

impl<A: Actor> Deref for ActorRef<A> {
    type Target = Address<A>;

    fn deref(&self) -> &Self::Target {
        &self.address
    }
}

impl<A: Actor> fmt::Debug for ActorRef<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActorRef")
            .field("actor", &self.address.actor_name())
            .finish_non_exhaustive()
    }
}

/// Handle to an actor thread.
///
/// Obtained from [`ActorRef::into_parts`]. `join` waits for the thread and
/// classifies a panic separately from normal shutdown. Dropping the handle
/// detaches the thread.
pub struct JoinHandle {
    actor: &'static str,
    handle: thread::JoinHandle<()>,
}

impl JoinHandle {
    /// Waits for the actor thread to finish.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Panicked`] when the thread unwound. The panic payload is
    /// converted to text; it is not exposed as a source chain because the
    /// original payload is not an `Error`.
    pub fn join(self) -> Result<()> {
        match self.handle.join() {
            Ok(()) => Ok(()),
            Err(payload) => Err(Error::Panicked {
                actor: self.actor,
                panic: panic_text(payload),
            }),
        }
    }

    /// Returns `true` once the actor thread has finished.
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
}

impl fmt::Debug for JoinHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinHandle")
            .field("actor", &self.actor)
            .field("finished", &self.handle.is_finished())
            .finish()
    }
}

/// Starts an actor with the default bounded mailbox.
///
/// # Errors
///
/// Returns [`Error::Spawn`] if the OS cannot create the actor thread. The
/// source preserves the underlying `io::Error`.
pub fn spawn<A: Actor>(actor: A) -> Result<ActorRef<A>> {
    spawn_with(actor, Some(DEFAULT_MAILBOX_CAPACITY))
}

/// Starts an actor with a bounded mailbox of the requested capacity.
///
/// When the mailbox is full, [`Address::tell`] blocks and
/// [`Address::try_tell`] reports [`Error::MailboxFull`].
///
/// # Errors
///
/// Returns [`Error::Spawn`] if the OS cannot create the actor thread.
pub fn spawn_bounded<A: Actor>(actor: A, capacity: NonZeroUsize) -> Result<ActorRef<A>> {
    spawn_with(actor, Some(capacity.get()))
}

/// Starts an actor with an unbounded mailbox.
///
/// Prefer [`spawn`] or [`spawn_bounded`]. An unbounded mailbox cannot apply
/// backpressure and may grow until the process runs out of memory.
///
/// # Errors
///
/// Returns [`Error::Spawn`] if the OS cannot create the actor thread.
pub fn spawn_unbounded<A: Actor>(actor: A) -> Result<ActorRef<A>> {
    spawn_with(actor, None)
}

fn spawn_with<A: Actor>(actor: A, capacity: Option<usize>) -> Result<ActorRef<A>> {
    let name = actor.name();
    let (sender, receiver) = match capacity {
        Some(capacity) => crossbeam_channel::bounded(capacity),
        None => unbounded(),
    };

    let handle = thread::Builder::new()
        .name(format!("actor-{name}"))
        .spawn(move || run(actor, receiver))
        .map_err(|source| Error::Spawn {
            actor: name,
            source,
        })?;

    Ok(ActorRef {
        address: Address::new(name, sender),
        join: JoinHandle {
            actor: name,
            handle,
        },
    })
}

fn run<A: Actor>(mut actor: A, receiver: Receiver<Envelope<A::Message>>) {
    let mut context = Context::new(actor.name());

    actor.started(&mut context);
    if !context.is_stopping() {
        while let Ok(envelope) = receiver.recv() {
            match envelope {
                Envelope::Message(message) => {
                    actor.handle(message, &mut context);
                    if context.is_stopping() {
                        break;
                    }
                }
                Envelope::Stop => break,
            }
        }
    }
    actor.stopped(&mut context);
}

fn short_type_name<T>() -> &'static str {
    let full = type_name::<T>();
    full.rsplit("::").next().unwrap_or(full)
}

fn panic_text(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}
