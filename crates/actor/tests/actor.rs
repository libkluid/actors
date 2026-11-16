//! Behavioural tests for the public actor API.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use actor::{Actor, ActorRef, Context, Error, ErrorKind, Operation, Reply};

struct Probe {
    value: u64,
    handled: Arc<AtomicUsize>,
}

enum Message {
    Add(u64),
    Value(Reply<u64>),
    DropReply(Reply<u64>),
    Sleep { delay: Duration, reply: Reply<u64> },
    Block { started: Reply<()>, delay: Duration },
    Stop,
    Panic,
}

impl Actor for Probe {
    type Message = Message;

    fn name(&self) -> &'static str {
        "probe"
    }

    fn handle(&mut self, message: Self::Message, ctx: &mut Context<Self>) {
        match message {
            Message::Add(amount) => {
                self.value = self.value.saturating_add(amount);
                self.handled.fetch_add(1, Ordering::SeqCst);
            }
            Message::Value(reply) => {
                let _ = reply.send(self.value);
            }
            Message::DropReply(_reply) => {
                // Dropping the handle is what the caller observes as NoReply.
            }
            Message::Sleep { delay, reply } => {
                thread::sleep(delay);
                let _ = reply.send(self.value);
            }
            Message::Block { started, delay } => {
                let _ = started.send(());
                thread::sleep(delay);
            }
            Message::Stop => ctx.stop(),
            Message::Panic => panic!("probe was asked to panic"),
        }
    }
}

fn capacity(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("test capacity is non-zero")
}

fn probe() -> (ActorRef<Probe>, Arc<AtomicUsize>) {
    probe_bounded(16)
}

fn probe_bounded(size: usize) -> (ActorRef<Probe>, Arc<AtomicUsize>) {
    let handled = Arc::new(AtomicUsize::new(0));
    let actor = Probe {
        value: 0,
        handled: Arc::clone(&handled),
    };
    let actor = actor::spawn_bounded(actor, capacity(size)).expect("probe starts");
    (actor, handled)
}

#[test]
fn tell_messages_are_fifo_and_ask_observes_them() {
    let (probe, handled) = probe();

    for _ in 0..1_000 {
        probe.tell(Message::Add(1)).expect("message is queued");
    }

    let value = probe.ask(Message::Value).expect("value is returned");
    assert_eq!(value, 1_000);
    assert_eq!(handled.load(Ordering::SeqCst), 1_000);

    probe.shutdown().expect("probe stops");
}

#[test]
fn ask_timeout_reports_the_reply_stage() {
    let (probe, _handled) = probe();

    let error = probe
        .ask_timeout(Duration::from_millis(20), |reply| Message::Sleep {
            delay: Duration::from_millis(150),
            reply,
        })
        .expect_err("the handler is slower than the deadline");

    assert!(matches!(
        error,
        Error::Timeout {
            operation: Operation::Reply,
            ..
        }
    ));
    assert_eq!(error.kind(), ErrorKind::Timeout);
    assert!(error.is_retryable());

    probe
        .shutdown()
        .expect("probe stops after the handler finishes");
}

#[test]
fn dropped_reply_is_reported_as_no_reply() {
    let (probe, _handled) = probe();

    let error = probe
        .ask(Message::DropReply)
        .expect_err("dropping the reply handle must not block the caller");

    assert!(matches!(error, Error::NoReply { actor: "probe" }));
    assert_eq!(error.kind(), ErrorKind::NoReply);
    assert!(!error.is_retryable());

    probe.shutdown().expect("probe stops");
}

#[test]
fn full_mailbox_is_reported_as_overload() {
    let (probe, _handled) = probe_bounded(1);

    // Block replies before sleeping, so the actor is provably busy and the
    // mailbox is empty when this returns.
    probe
        .ask(|started| Message::Block {
            started,
            delay: Duration::from_millis(150),
        })
        .expect("the blocking handler starts");

    probe
        .try_tell(Message::Add(1))
        .expect("one slot is available");

    let error = probe
        .try_tell(Message::Add(1))
        .expect_err("the one-slot mailbox is full");

    assert!(matches!(error, Error::MailboxFull { actor: "probe" }));
    assert_eq!(error.kind(), ErrorKind::Overloaded);
    assert!(error.is_retryable());

    probe.shutdown().expect("probe drains and stops");
}

#[test]
fn send_after_shutdown_is_stopped() {
    let (probe, _handled) = probe();
    let address = probe.address().clone();

    probe.shutdown().expect("probe stops");

    let error = address
        .tell(Message::Add(1))
        .expect_err("the mailbox is closed");

    assert!(matches!(error, Error::Stopped { actor: "probe" }));
    assert_eq!(error.kind(), ErrorKind::Stopped);
    assert!(!error.is_retryable());
}

#[test]
fn ask_after_shutdown_is_stopped() {
    let (probe, _handled) = probe();
    let address = probe.address().clone();

    probe.shutdown().expect("probe stops");

    let error = address
        .ask(Message::Value)
        .expect_err("the mailbox is closed");

    assert!(matches!(error, Error::Stopped { actor: "probe" }));
}

#[test]
fn panic_is_classified_by_join() {
    let (probe, _handled) = probe();

    probe.tell(Message::Panic).expect("message is queued");

    let error = probe.shutdown().expect_err("the actor thread panicked");
    assert!(matches!(
        error,
        Error::Panicked { actor: "probe", ref panic } if panic.contains("probe was asked to panic")
    ));
    assert_eq!(error.kind(), ErrorKind::Panicked);
    assert!(!error.is_retryable());
}

#[test]
fn shutdown_drains_messages_queued_before_stop() {
    let (probe, handled) = probe();

    for _ in 0..500 {
        probe.tell(Message::Add(1)).expect("message is queued");
    }
    probe.shutdown().expect("probe drains and stops");

    assert_eq!(handled.load(Ordering::SeqCst), 500);
}

#[test]
fn dropping_the_last_address_drains_and_stops_the_actor() {
    let (probe, handled) = probe();
    let (address, join) = probe.into_parts();

    for _ in 0..250 {
        address.tell(Message::Add(1)).expect("message is queued");
    }
    drop(address);

    join.join().expect("the actor stops when senders disappear");
    assert_eq!(handled.load(Ordering::SeqCst), 250);
}

#[test]
fn context_stop_drops_later_messages() {
    let (probe, handled) = probe_bounded(64);

    probe.tell(Message::Add(1)).expect("message is queued");
    probe
        .ask(|started| Message::Block {
            started,
            delay: Duration::from_millis(100),
        })
        .expect("the blocking handler starts");

    // The actor is sleeping, so the mailbox is still open while these messages
    // are queued behind the stop request.
    probe.tell(Message::Stop).expect("stop is queued");
    for _ in 0..50 {
        probe.tell(Message::Add(1)).expect("message is queued");
    }

    probe.shutdown().expect("probe stops");
    assert_eq!(handled.load(Ordering::SeqCst), 1);
}

#[test]
fn unbounded_mailbox_has_no_capacity() {
    let handled = Arc::new(AtomicUsize::new(0));
    let actor = Probe { value: 0, handled };
    let probe = actor::spawn_unbounded(actor).expect("probe starts");

    assert_eq!(probe.capacity(), None);
    for _ in 0..100 {
        probe.tell(Message::Add(1)).expect("message is queued");
    }
    assert_eq!(probe.ask(Message::Value).expect("value is returned"), 100);

    probe.shutdown().expect("probe stops");
}
