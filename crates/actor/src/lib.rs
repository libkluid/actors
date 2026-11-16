//! A minimal, typed actor runtime built on `crossbeam-channel`.
//!
//! The runtime gives every actor its own OS thread, a bounded or unbounded
//! mailbox, and two communication styles:
//!
//! - [`Address::tell`] for fire-and-forget messages
//! - [`Address::ask`] / [`Address::ask_timeout`] for request/response
//!
//! Messages are typed through [`Actor::Message`], so a request cannot be sent
//! to the wrong actor and a reply cannot have the wrong type. Handler-level
//! failures travel back as the payload of a [`Reply`]; [`Error`] only describes
//! mailbox and lifecycle failures.
//!
//! # Example
//!
//! ```
//! use actor::{Actor, Context, Reply};
//!
//! struct Greeter;
//!
//! enum Message {
//!     Hello(Reply<String>),
//! }
//!
//! impl Actor for Greeter {
//!     type Message = Message;
//!
//!     fn handle(&mut self, message: Message, _ctx: &mut Context<Self>) {
//!         match message {
//!             Message::Hello(reply) => {
//!                 let _ = reply.send("hello".to_owned());
//!             }
//!         }
//!     }
//! }
//!
//! let greeter = actor::spawn(Greeter).expect("actor thread starts");
//! let greeting = greeter.ask(Message::Hello).expect("actor answers");
//! assert_eq!(greeting, "hello");
//! greeter.shutdown().expect("actor stops cleanly");
//! ```
//!
//! # Guarantees and non-goals
//!
//! - Messages from one sender are delivered in FIFO order.
//! - A stopped or panicked actor is reported as [`Error::Stopped`] or
//!   [`Error::Panicked`]; it is never silently restarted.
//! - Delivery is at-most-once. The runtime does not retry messages, because it
//!   cannot know whether an operation is idempotent.
//! - There is no supervision tree, remote transport, or async integration. See
//!   the workspace README for the extension points.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod address;
mod error;
mod reply;
mod runtime;

pub use address::Address;
pub use error::{Error, ErrorKind, Operation, Result};
pub use reply::{Reply, ReplyDropped};
pub use runtime::{
    Actor, ActorRef, Context, DEFAULT_MAILBOX_CAPACITY, JoinHandle, spawn, spawn_bounded,
    spawn_unbounded,
};
