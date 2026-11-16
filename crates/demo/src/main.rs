//! Command-line front end for the actor PoC.
//!
//! This is the composition root: it parses argv, starts actors, and is the only
//! place where failures become user-facing text and exit codes. Runtime errors
//! and domain errors keep their types all the way up so [`exit_code`] can map
//! them; `anyhow` only adds human context.

#![forbid(unsafe_code)]

mod args;
mod chaos;
mod scenarios;

use std::process::ExitCode;

use actor::ErrorKind;
use anyhow::Context;
use args::{Args, Command};

fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            // Invalid invocation is not a runtime failure. Print usage and use
            // the conventional usage exit code instead of an error chain.
            eprintln!("error: {error}");
            eprintln!();
            eprintln!("{}", args::USAGE);
            return ExitCode::from(2);
        }
    };

    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Printed exactly once, with every `context` layer and `source`.
            eprintln!("error: {error:?}");
            ExitCode::from(exit_code(&error))
        }
    }
}

fn run(args: Args) -> anyhow::Result<()> {
    match args.command {
        Command::Run {
            producers,
            per_producer,
            capacity,
        } => scenarios::run(producers, per_producer, capacity)
            .context("could not run the concurrent counter scenario"),
        Command::Fail => scenarios::fail().context("could not run the domain error scenario"),
        Command::Panic => scenarios::panicked().context("could not run the panic scenario"),
        Command::Timeout => scenarios::timeout().context("could not run the timeout scenario"),
        Command::Saturate => {
            scenarios::saturate().context("could not run the backpressure scenario")
        }
        Command::Stopped => {
            scenarios::stopped().context("could not run the stopped actor scenario")
        }
    }
}

/// Maps a failure to a `sysexits.h`-style process exit code.
///
/// `anyhow::Error::downcast_ref` sees through `.context()` layers, so typed
/// errors stay available at the top level. This is the only place that turns
/// error kinds into policy.
fn exit_code(error: &anyhow::Error) -> u8 {
    if error.downcast_ref::<counter::Error>().is_some() {
        return 65; // EX_DATAERR: the actor rejected the input
    }

    let Some(error) = error.downcast_ref::<actor::Error>() else {
        return 1;
    };

    match error.kind() {
        ErrorKind::Stopped => 69,                         // EX_UNAVAILABLE
        ErrorKind::Timeout | ErrorKind::Overloaded => 75, // EX_TEMPFAIL
        ErrorKind::NoReply | ErrorKind::Panicked => 70,   // EX_SOFTWARE
        ErrorKind::Spawn => 71,                           // EX_OSERR
        _ => 1,
    }
}
