use self::child::Child;
use crate::messages::*;
use futures::prelude::*;
use snafu::prelude::*;
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    io::{BufWriter, Write},
    ops::ControlFlow,
};
use tokio::{
    sync::mpsc,
    task::{self, JoinHandle},
};
use tracing::trace;

mod child;

/// Run the providence server
#[derive(argh::FromArgs)]
#[argh(subcommand, name = "server")]
pub struct Config {}

pub async fn main(_config: Config) -> Result<()> {
    let (stdin_task, mut rx) = spawn_stdin();
    let (stdout_task, tx) = spawn_stdout();

    let mut children = BTreeMap::new();
    while let Some(msg) = rx.recv().await {
        use ServerMessage as Msg;

        match msg {
            Msg::Spawn(Spawn { psid, command }) => {
                // TODO: perhaps a cap on running processes?

                let child = Child::spawn(psid, &command, &tx).context(UnableToSpawnChildSnafu)?;
                let old_child = children.insert(psid, child);
                maybe_kill(old_child)
                    .await
                    .context(UnableToKillReplacedChildSnafu { psid })?;

                tx.send(ChildStarted { psid }.into())
                    .await
                    .context(UnableToRelayChildStartedToClientSnafu)?;
            }
            Msg::Kill(Kill { psid }) => {
                let child = children.remove(&psid);
                maybe_kill(child)
                    .await
                    .context(UnableToKillChildSnafu { psid })?;
            }
            Msg::Input(i @ Input { psid, .. }) => {
                trace!("submitting input {i:?}");

                let child = children.get(&psid).context(ChildNotRunningSnafu { psid })?;
                child
                    .input(i)
                    .await
                    .context(UnableToRelayInputToChildSnafu)?;
            }
        }
    }

    trace!("server main loop exiting");

    drop(tx);
    drop(rx);

    let children = future::join_all(children.into_values().map(Child::force_shutdown));

    let (children, stdout_task, stdin_task) = futures::join!(children, stdout_task, stdin_task,);

    // TODO: Join the errors when there are multiple?

    for child in children {
        child.context(UnableToShutdownChildSnafu)?;
    }

    stdout_task.context(UnableToJoinStdoutTaskSnafu)??;
    stdin_task.context(UnableToJoinStdinTaskSnafu)??;

    Ok(())
}

fn spawn_stdin() -> (JoinHandle<Result<()>>, mpsc::Receiver<ServerMessage>) {
    let (tx, rx) = mpsc::channel(10);

    let child = task::spawn_blocking(move || {
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock();
        // StdinLock is naturally buffered

        loop {
            let msg = match crate::rmp_final_read(rmp_serde::decode::from_read(&mut stdin)) {
                ControlFlow::Continue(msg) => msg,
                ControlFlow::Break(e) => break e.context(UnableToDecodeServerMessageSnafu),
            };

            trace!("decoded message {msg:?}");
            tx.blocking_send(msg)
                .context(UnableToSendServerMessageSnafu)?;
        }
    });

    (child, rx)
}

fn spawn_stdout() -> (JoinHandle<Result<()>>, mpsc::Sender<ClientMessage>) {
    let (tx, mut rx) = mpsc::channel(10);

    let child = task::spawn_blocking(move || {
        let stdout = std::io::stdout();
        let stdout = stdout.lock();
        let mut stdout = BufWriter::new(stdout);

        while let Some(msg) = rx.blocking_recv() {
            rmp_serde::encode::write(&mut stdout, &msg)
                .context(UnableToEncodeClientMessageSnafu)?;
            stdout.flush().context(UnableToFlushClientMessageSnafu)?;
        }

        Ok(())
    });

    (child, tx)
}

async fn maybe_kill(child: Option<Child>) -> child::Result<bool> {
    match child {
        Some(child) => {
            trace!("killing running child");

            child.force_shutdown().await?;

            Ok(true)
        }
        None => {
            trace!("ignoring requested kill");
            Ok(false)
        }
    }
}

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("Could not decode the message from the client"))]
    UnableToDecodeServerMessage {
        source: rmp_serde::decode::Error,
    },

    #[snafu(display("Could not transmit the client's message to core loop"))]
    UnableToSendServerMessage {
        source: mpsc::error::SendError<ServerMessage>,
    },

    #[snafu(display("Could not encode the message to the client"))]
    UnableToEncodeClientMessage {
        source: rmp_serde::encode::Error,
    },

    #[snafu(display("Could not flush message to the client"))]
    UnableToFlushClientMessage {
        source: std::io::Error,
    },

    #[snafu(display("The child is not currently running ({psid})"))]
    ChildNotRunning {
        psid: ProcessSequenceId,
    },

    #[snafu(display("Could not kill the child process being replaced ({psid})"))]
    UnableToKillReplacedChild {
        source: child::Error,
        psid: ProcessSequenceId,
    },

    #[snafu(display("Could not kill the currently-running child process ({psid})"))]
    UnableToKillChild {
        source: child::Error,
        psid: ProcessSequenceId,
    },

    UnableToShutdownChild {
        source: child::Error,
    },

    UnableToSpawnChild {
        source: child::Error,
    },

    UnableToRelayChildStartedToClient {
        source: mpsc::error::SendError<ClientMessage>,
    },

    UnableToRelayInputToChild {
        source: child::Error,
    },

    #[snafu(display("Could not join to the stdin task"))]
    UnableToJoinStdinTask {
        source: task::JoinError,
    },

    #[snafu(display("Could not join to the stdout task"))]
    UnableToJoinStdoutTask {
        source: task::JoinError,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

fn display_command(s: &OsStr) -> impl std::fmt::Display + '_ {
    s.to_str().unwrap_or("<non-UTF-8 command>")
}
