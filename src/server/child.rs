use super::display_command;
use crate::messages::*;
use snafu::prelude::*;
use std::{
    ffi::{OsStr, OsString},
    process::Stdio,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{ChildStdin, Command},
    sync::{mpsc, oneshot},
    task::{self, JoinHandle},
};
use tracing::{trace, trace_span, Instrument};

#[derive(Debug)]
pub struct Child {
    stdin_tx: mpsc::Sender<Input>,
    stdin_task: JoinHandle<Result<()>>,
    stdout_task: JoinHandle<Result<(), ChildOutputError>>,
    stderr_task: JoinHandle<Result<(), ChildOutputError>>,
    kill_tx: Option<oneshot::Sender<()>>,
    child_task: JoinHandle<Result<()>>,
}

impl Child {
    pub fn spawn(
        psid: ProcessSequenceId,
        command: &OsStr,
        tx: &mpsc::Sender<ClientMessage>,
    ) -> Result<Self> {
        trace!("starting child `{}`", display_command(command));

        let mut child = Command::new(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context(UnableToSpawnChildProcessSnafu { command })?;

        let stdin = child.stdin.take().context(ChildHasNoStdinSnafu)?;
        let stdout = child.stdout.take().context(ChildHasNoStdoutSnafu)?;
        let stderr = child.stderr.take().context(ChildHasNoStderrSnafu)?;

        let (stdin_task, stdin_tx) = Self::spawn_input_task(stdin);
        let stdout_task =
            Self::spawn_output_task(psid, stdout, Output::stdout, tx.clone(), "stdout");
        let stderr_task =
            Self::spawn_output_task(psid, stderr, Output::stderr, tx.clone(), "stderr");

        let (child_task, kill_tx) = Self::spawn_child_task(psid, child, tx);
        let kill_tx = Some(kill_tx);

        Ok(Self {
            stdin_tx,
            stdin_task,
            stdout_task,
            stderr_task,
            kill_tx,
            child_task,
        })
    }

    fn spawn_input_task(mut stdin: ChildStdin) -> (JoinHandle<Result<()>>, mpsc::Sender<Input>) {
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<Input>(10);

        let stdin_task = tokio::spawn(async move {
            while let Some(input) = stdin_rx.recv().await {
                stdin
                    .write_all(&input.content)
                    .await
                    .context(UnableToWriteInputToChildSnafu)?;
                stdin
                    .flush()
                    .await
                    .context(UnableToFlushInputToChildSnafu)?;
            }

            Ok(())
        });

        (stdin_task, stdin_tx)
    }

    fn spawn_output_task(
        psid: ProcessSequenceId,
        mut io_handle: impl AsyncRead + Unpin + Send + 'static,
        into_output: impl Fn(ProcessSequenceId, Vec<u8>) -> Output + Send + 'static,
        tx: mpsc::Sender<ClientMessage>,
        id: &'static str,
    ) -> JoinHandle<Result<(), ChildOutputError>> {
        tokio::spawn({
            async move {
                loop {
                    // TODO: How do we handle partial UTF-8 reads?
                    trace!("output task waiting for data");
                    let mut data = Vec::with_capacity(1024);
                    let n_bytes = io_handle
                        .read_buf(&mut data)
                        .await
                        .context(UnableToReadSnafu)?;

                    trace!("output task got {n_bytes} bytes of data");

                    if n_bytes == 0 {
                        trace!("output task exiting");
                        break;
                    }

                    let cmd = into_output(psid, data).into();
                    trace!("output task sending {cmd:?}");
                    tx.send(cmd).await.context(UnableToRelayOutputSnafu)?;
                }

                Ok(())
            }
            .instrument(trace_span!("output", id))
        })
    }

    fn spawn_child_task(
        psid: ProcessSequenceId,
        mut child: tokio::process::Child,
        tx: &mpsc::Sender<ClientMessage>,
    ) -> (JoinHandle<Result<()>>, oneshot::Sender<()>) {
        let (kill_tx, kill_rx) = oneshot::channel::<()>();

        let child_task = tokio::spawn({
            let tx = tx.clone();
            async move {
                tokio::select! {
                    k = kill_rx => {
                        k.context(UnableToReceiveKillFromServerSnafu)?;
                        child.kill().await.context(UnableToKillChildSnafu)?
                    },
                    w = child.wait() => {
                        w.context(UnableToWaitForChildSnafu)?;
                    },
                };

                tx.send(ChildStopped { psid }.into())
                    .await
                    .context(UnableToRelayChildStoppedToServerSnafu)?;

                Ok(())
            }
        });

        (child_task, kill_tx)
    }

    pub async fn kill(&mut self) -> Result<()> {
        if let Some(kill_tx) = self.kill_tx.take() {
            kill_tx
                .send(())
                .ok()
                .context(UnableToRelayKillMessageSnafu)?;
        }

        Ok(())
    }

    pub async fn input(&self, input: Input) -> Result<()> {
        self.stdin_tx
            .send(input)
            .await
            .context(UnableToRelayInputMessageFromClientSnafu)
    }

    pub async fn force_shutdown(mut self) -> Result<()> {
        self.kill().await?;
        self.shutdown().await
    }

    pub async fn shutdown(self) -> Result<()> {
        let Self {
            stdin_tx,
            stdin_task,
            stdout_task,
            stderr_task,
            kill_tx,
            child_task,
        } = self;

        drop(stdin_tx);
        drop(kill_tx);

        let (stdin_task, stdout_task, stderr_task, child_task) =
            futures::join!(stdin_task, stdout_task, stderr_task, child_task);

        stdin_task.context(UnableToJoinChildStdinTaskSnafu)??;
        stdout_task
            .context(UnableToJoinChildStdoutTaskSnafu)?
            .context(UnableToProcessChildStdoutSnafu)?;
        stderr_task
            .context(UnableToJoinChildStderrTaskSnafu)?
            .context(UnableToProcessChildStderrSnafu)?;
        child_task.context(UnableToJoinChildTaskSnafu)??;

        Ok(())
    }
}

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display(r#"Could not spawn the process `{}`"#, display_command(command)))]
    UnableToSpawnChildProcess {
        source: std::io::Error,
        command: OsString,
    },

    UnableToRelayKillMessage,

    #[snafu(display(r#"Could not kill the currently-running child process"#))]
    UnableToKillChild {
        source: std::io::Error,
    },

    UnableToWaitForChild {
        source: std::io::Error,
    },

    UnableToRelayChildStoppedToServer {
        source: mpsc::error::SendError<ClientMessage>,
    },

    #[snafu(display("The child has no stdin handle"))]
    ChildHasNoStdin,

    #[snafu(display("The child has no stdout handle"))]
    ChildHasNoStdout,

    #[snafu(display("The child has no stderr handle"))]
    ChildHasNoStderr,

    UnableToReceiveKillFromServer {
        source: oneshot::error::RecvError,
    },

    #[snafu(display("Could not relay input to the child"))]
    UnableToRelayInputMessageFromClient {
        source: mpsc::error::SendError<Input>,
    },

    #[snafu(display("Could not write input to the child"))]
    UnableToWriteInputToChild {
        source: std::io::Error,
    },

    #[snafu(display("Could not flush input to the child"))]
    UnableToFlushInputToChild {
        source: std::io::Error,
    },

    #[snafu(display("Could not join to the child stdin task"))]
    UnableToJoinChildStdinTask {
        source: task::JoinError,
    },

    #[snafu(display("Could not join to the child stdout task"))]
    UnableToJoinChildStdoutTask {
        source: task::JoinError,
    },

    #[snafu(display("Could not process child stdout"))]
    UnableToProcessChildStdout {
        source: ChildOutputError,
    },

    #[snafu(display("Could not join to the child stderr task"))]
    UnableToJoinChildStderrTask {
        source: task::JoinError,
    },

    #[snafu(display("Could not process child stderr"))]
    UnableToProcessChildStderr {
        source: ChildOutputError,
    },

    #[snafu(display("Could not join to the child task"))]
    UnableToJoinChildTask {
        source: task::JoinError,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Snafu)]
pub enum ChildOutputError {
    UnableToRead {
        source: std::io::Error,
    },

    UnableToRelayOutput {
        source: mpsc::error::SendError<ClientMessage>,
    },
}
