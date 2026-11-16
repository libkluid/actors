//! argv parsing and usage errors.
//!
//! Argument errors are kept separate from runtime failures because they are
//! handled differently: print usage and exit with code 2, never log an error
//! chain.

use std::num::NonZeroUsize;

use thiserror::Error;

/// Usage text printed for invalid invocations.
pub const USAGE: &str = "\
usage: actors-demo <COMMAND>

commands:
  run        drive one counter actor from concurrent producer threads
  fail       return a domain error from inside a handler
  panic      crash the actor thread and report the join error
  timeout    ask a slow actor with a short deadline
  saturate   fill a bounded mailbox to force backpressure
  stopped    send to an actor after it has shut down

run options:
  --producers <N>       producer threads (default: 4)
  --per-producer <N>    increments per producer (default: 10000)
  --capacity <N>        mailbox capacity (default: 1024)";

/// Parsed command line.
#[derive(Debug)]
pub struct Args {
    /// Selected scenario.
    pub command: Command,
}

/// Supported scenarios.
#[derive(Debug)]
pub enum Command {
    /// Concurrent producers feeding one actor.
    Run {
        /// Number of producer threads.
        producers: NonZeroUsize,
        /// Increments performed by each producer.
        per_producer: NonZeroUsize,
        /// Mailbox capacity for the counter.
        capacity: NonZeroUsize,
    },
    /// A handler-level domain error returned through a reply.
    Fail,
    /// A handler panic reported through the join handle.
    Panic,
    /// An `ask` deadline that expires while the handler is busy.
    Timeout,
    /// A full bounded mailbox reported by `try_tell`.
    Saturate,
    /// A send attempted after the actor has exited.
    Stopped,
}

/// Errors raised while parsing argv.
///
/// These never need a source chain: the message already identifies the flag and
/// value the user has to fix.
#[derive(Debug, Error)]
pub enum UsageError {
    /// No subcommand was supplied.
    #[error("no command given")]
    MissingCommand,

    /// The subcommand is not part of the vocabulary.
    #[error("unknown command `{0}`")]
    UnknownCommand(String),

    /// A flag was supplied without its value.
    #[error("`{flag}` needs a value")]
    MissingValue {
        /// Flag that was missing a value.
        flag: String,
    },

    /// A flag is not part of the command's vocabulary.
    #[error("unknown option `{0}`")]
    UnknownOption(String),

    /// A flag value could not be parsed.
    #[error("invalid value `{value}` for `{flag}`: {reason}")]
    InvalidValue {
        /// Flag being parsed.
        flag: &'static str,
        /// Rejected input.
        value: String,
        /// Why the input was rejected.
        reason: String,
    },

    /// A command that takes no arguments received one.
    #[error("`{command}` does not accept arguments")]
    UnexpectedArgument {
        /// Command that was invoked.
        command: &'static str,
        /// Rejected argument.
        argument: String,
    },
}

impl Args {
    /// Parses arguments after the program name.
    ///
    /// # Errors
    ///
    /// Returns [`UsageError`] for an unknown command, a missing flag value, an
    /// unknown flag, or a non-positive numeric value.
    pub fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, UsageError> {
        let command = args.next().ok_or(UsageError::MissingCommand)?;
        let command = match command.as_str() {
            "run" => parse_run(&mut args)?,
            "fail" => {
                ensure_no_arguments("fail", &mut args)?;
                Command::Fail
            }
            "panic" => {
                ensure_no_arguments("panic", &mut args)?;
                Command::Panic
            }
            "timeout" => {
                ensure_no_arguments("timeout", &mut args)?;
                Command::Timeout
            }
            "saturate" => {
                ensure_no_arguments("saturate", &mut args)?;
                Command::Saturate
            }
            "stopped" => {
                ensure_no_arguments("stopped", &mut args)?;
                Command::Stopped
            }
            other => return Err(UsageError::UnknownCommand(other.to_owned())),
        };

        Ok(Self { command })
    }
}

fn parse_run(args: &mut impl Iterator<Item = String>) -> Result<Command, UsageError> {
    let mut producers = 4_usize;
    let mut per_producer = 10_000_usize;
    let mut capacity = 1024_usize;

    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| UsageError::MissingValue { flag: flag.clone() })?;

        match flag.as_str() {
            "--producers" => producers = parse_positive("--producers", &value)?,
            "--per-producer" => per_producer = parse_positive("--per-producer", &value)?,
            "--capacity" => capacity = parse_positive("--capacity", &value)?,
            other => return Err(UsageError::UnknownOption(other.to_owned())),
        }
    }

    Ok(Command::Run {
        producers: non_zero("--producers", producers)?,
        per_producer: non_zero("--per-producer", per_producer)?,
        capacity: non_zero("--capacity", capacity)?,
    })
}

fn parse_positive(flag: &'static str, value: &str) -> Result<usize, UsageError> {
    value
        .parse::<usize>()
        .map_err(|error| UsageError::InvalidValue {
            flag,
            value: value.to_owned(),
            reason: error.to_string(),
        })
}

fn non_zero(flag: &'static str, value: usize) -> Result<NonZeroUsize, UsageError> {
    NonZeroUsize::new(value).ok_or_else(|| UsageError::InvalidValue {
        flag,
        value: value.to_string(),
        reason: "must be greater than zero".to_owned(),
    })
}

fn ensure_no_arguments(
    command: &'static str,
    args: &mut impl Iterator<Item = String>,
) -> Result<(), UsageError> {
    if let Some(argument) = args.next() {
        return Err(UsageError::UnexpectedArgument { command, argument });
    }
    Ok(())
}
