//! A deliberately unreliable actor used to exercise runtime failure paths.
//!
//! The counter actor is well-behaved. This actor exists so the demo can produce
//! a slow handler, a full mailbox, and a panic without polluting the domain
//! library with test-only commands.

use std::num::NonZeroUsize;
use std::thread;
use std::time::Duration;

use actor::{Actor, ActorRef, Context, Reply};

/// Failure-injection commands.
#[derive(Debug)]
pub enum Message {
    /// Sleeps before answering, to trigger a caller timeout.
    Slow {
        /// How long the handler blocks.
        delay: Duration,
        /// Reply sent after the sleep.
        reply: Reply<()>,
    },

    /// Signals the caller and then keeps the handler busy.
    ///
    /// The early signal lets the test fill the mailbox while the actor is known
    /// to be outside of `recv`.
    Block {
        /// Reply sent before the sleep.
        started: Reply<()>,
        /// How long the handler blocks after signalling.
        delay: Duration,
    },

    /// Does nothing; used to occupy mailbox slots.
    Noop,

    /// Panics on the actor thread.
    Panic,
}

/// An actor with no state whose only purpose is failure injection.
#[derive(Debug, Default)]
pub struct Chaos;

impl Actor for Chaos {
    type Message = Message;

    fn name(&self) -> &'static str {
        "chaos"
    }

    #[allow(clippy::panic)] // the crash is the scenario being demonstrated
    fn handle(&mut self, message: Self::Message, _ctx: &mut Context<Self>) {
        match message {
            Message::Slow { delay, reply } => {
                thread::sleep(delay);
                let _ = reply.send(());
            }
            Message::Block { started, delay } => {
                let _ = started.send(());
                thread::sleep(delay);
            }
            Message::Noop => {}
            Message::Panic => panic!("chaos actor was asked to panic"),
        }
    }
}

/// Starts the chaos actor with the default mailbox.
///
/// # Errors
///
/// Returns [`actor::Error::Spawn`] if the actor thread cannot be created.
pub fn spawn() -> actor::Result<ActorRef<Chaos>> {
    actor::spawn(Chaos)
}

/// Starts the chaos actor with a bounded mailbox.
///
/// # Errors
///
/// Returns [`actor::Error::Spawn`] if the actor thread cannot be created.
pub fn spawn_bounded(capacity: NonZeroUsize) -> actor::Result<ActorRef<Chaos>> {
    actor::spawn_bounded(Chaos, capacity)
}
