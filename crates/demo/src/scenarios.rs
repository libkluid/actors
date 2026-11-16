//! Runnable scenarios that exercise one failure path each.

use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use actor::Operation;
use anyhow::{Context, anyhow, bail, ensure};
use counter::Message;

use crate::chaos::{self, Message as ChaosMessage};

/// Concurrent producers feed one counter, then the final value is read.
///
/// Producers only use `tell`, so the bounded mailbox provides backpressure
/// instead of unbounded buffering.
pub fn run(
    producers: NonZeroUsize,
    per_producer: NonZeroUsize,
    capacity: NonZeroUsize,
) -> anyhow::Result<()> {
    let counter = counter::spawn(capacity).context("could not start the counter actor")?;
    let address = counter.address().clone();
    let producers_count = producers.get();
    let per_producer_count = per_producer.get();
    let started = Instant::now();

    std::thread::scope(|scope| -> anyhow::Result<()> {
        let mut handles = Vec::with_capacity(producers_count);

        for _ in 0..producers_count {
            let address = address.clone();
            handles.push(scope.spawn(move || -> actor::Result<()> {
                for _ in 0..per_producer_count {
                    address.tell(Message::Add(1))?;
                }
                Ok(())
            }));
        }

        for (index, handle) in handles.into_iter().enumerate() {
            handle
                .join()
                .map_err(|_| anyhow!("producer {index} panicked"))?
                .with_context(|| format!("producer {index} stopped before finishing"))?;
        }

        Ok(())
    })?;

    let total = counter
        .ask(Message::Value)
        .context("could not read the final counter value")?;
    let elapsed = started.elapsed();

    let expected = u64::try_from(producers_count)
        .ok()
        .zip(u64::try_from(per_producer_count).ok())
        .and_then(|(producers, per_producer)| producers.checked_mul(per_producer))
        .context("configured increment count does not fit in u64")?;
    ensure!(
        total == expected,
        "counter reported {total}, expected {expected}"
    );

    counter
        .shutdown()
        .context("could not shut down the counter actor")?;

    println!("producers: {producers_count} x {per_producer_count}");
    println!("counter:   {total}");
    println!("elapsed:   {elapsed:?}");
    Ok(())
}

/// Returns a domain error from a handler through `Reply<Result<u64, Error>>`.
///
/// The error is data on a successful transport: the actor stays alive and the
/// binary maps [`counter::Error`] to `EX_DATAERR`.
pub fn fail() -> anyhow::Result<()> {
    let counter =
        counter::spawn(default_capacity()).context("could not start the counter actor")?;

    let seeded = counter
        .ask(|reply| Message::AddChecked { amount: 1, reply })
        .context("could not seed the counter")?;
    seeded.context("could not seed the counter")?;

    let outcome = counter
        .ask(|reply| Message::AddChecked {
            amount: u64::MAX,
            reply,
        })
        .context("could not ask for an overflowing increment")?;

    counter
        .shutdown()
        .context("could not shut down the counter actor")?;

    outcome.context("the counter rejected the increment")?;
    bail!("the overflowing increment unexpectedly succeeded")
}

/// Crashes the actor and surfaces the panic from `JoinHandle::join`.
pub fn panicked() -> anyhow::Result<()> {
    // The default panic hook would print the panic at the crash site, and the
    // top-level reporter would print it again. Suppress the hook while this
    // scenario runs so the CLI reports the failure exactly once.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = panicked_inner();
    std::panic::set_hook(previous_hook);
    result
}

fn panicked_inner() -> anyhow::Result<()> {
    let actor = chaos::spawn().context("could not start the chaos actor")?;
    actor
        .tell(ChaosMessage::Panic)
        .context("could not queue the panic message")?;

    actor
        .shutdown()
        .context("the chaos actor did not shut down cleanly")?;
    bail!("the panic message unexpectedly left the actor running")
}

/// Asks a slow actor with a deadline shorter than the handler delay.
pub fn timeout() -> anyhow::Result<()> {
    let actor = chaos::spawn().context("could not start the chaos actor")?;

    let outcome = actor.ask_timeout(Duration::from_millis(50), |reply| ChaosMessage::Slow {
        delay: Duration::from_millis(250),
        reply,
    });

    // The handler keeps running after the timeout, so stop and join even when
    // the request failed.
    let shutdown = actor
        .shutdown()
        .context("could not shut down the chaos actor");

    match (outcome, shutdown) {
        (Err(error), Ok(())) => {
            ensure!(
                matches!(
                    error,
                    actor::Error::Timeout {
                        operation: Operation::Reply,
                        ..
                    }
                ),
                "expected a reply timeout, got: {error}"
            );
            Err::<(), _>(error).context("the slow request did not finish in time")
        }
        (Ok(()), Ok(())) => bail!("the slow request unexpectedly finished within the deadline"),
        (_, Err(error)) => Err(error),
    }
}

/// Fills a one-slot mailbox while the actor is busy and reports overload.
pub fn saturate() -> anyhow::Result<()> {
    let capacity = NonZeroUsize::new(1).unwrap_or(NonZeroUsize::MIN);
    let actor = chaos::spawn_bounded(capacity).context("could not start the chaos actor")?;

    // `Block` replies before sleeping, so this returns while the actor is known
    // to be busy and the mailbox is empty.
    actor
        .ask(|started| ChaosMessage::Block {
            started,
            delay: Duration::from_millis(200),
        })
        .context("could not start the blocking handler")?;

    actor
        .try_tell(ChaosMessage::Noop)
        .context("the mailbox rejected the first message")?;

    let overload = match actor.try_tell(ChaosMessage::Noop) {
        Ok(()) => bail!("the second message unexpectedly fit in a full mailbox"),
        Err(error) => error,
    };

    actor
        .shutdown()
        .context("could not shut down the chaos actor")?;

    Err::<(), _>(overload).context("the bounded mailbox is full")
}

/// Sends to an actor after a clean shutdown.
pub fn stopped() -> anyhow::Result<()> {
    let actor = chaos::spawn().context("could not start the chaos actor")?;
    let address = actor.address().clone();

    actor
        .shutdown()
        .context("could not shut down the chaos actor")?;

    address
        .tell(ChaosMessage::Noop)
        .context("could not send to the stopped actor")?;

    bail!("the send to a stopped actor unexpectedly succeeded")
}

fn default_capacity() -> NonZeroUsize {
    NonZeroUsize::new(1024).unwrap_or(NonZeroUsize::MIN)
}
