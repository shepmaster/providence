use crate::{
    messages::{
        ChildStarted, ChildStopped, ClientMessage, Input, Kill, Output, ProcessSequenceId,
        ServerMessage, Spawn,
    },
    rmp_final_read,
};
use futures::prelude::*;
use snafu::prelude::*;
use std::{
    io::{BufReader, BufWriter, Write, Stderr},
    ops::ControlFlow,
    process::Stdio,
    time::Duration, collections::BTreeMap,
};
use tokio::{
    process::{self, Command},
    sync::{mpsc, oneshot},
    task, time,
};
use tokio_util::io::SyncIoBridge;
use tracing::trace;

/// Run the providence client
#[derive(argh::FromArgs)]
#[argh(subcommand, name = "client")]
pub struct Config {}

// TODO: spawn through docker
// TODO: spawn as non-root

pub async fn main(_config: Config) -> Result<()> {
    // let (client, mut messages) = Client::spawn()?;

    // tokio::spawn(async move {
    //     trace!("message task booted");

    //     while let Some(msg) = messages.recv().await {
    //         use ClientMessage as Msg;

    //         match msg {
    //             Msg::ChildStarted(ChildStarted { psid }) => eprintln!("[{psid}] child started"),
    //             Msg::ChildStopped(ChildStopped { psid }) => eprintln!("[{psid}] child stopped"),
    //             Msg::Output(Output { psid, stdout, .. }) => {
    //                 if let Ok(s) = std::str::from_utf8(&stdout) {
    //                     eprint!("[{psid}]> {s}");
    //                 }
    //             }
    //         }
    //     }
    // });

    // let ids = (0..10).map(ProcessSequenceId::new);

    // let work = ids.clone().map(|psid| {
    //     let client = &client;

    //     async move {
    //         let command = "/Users/shep/Projects/providence/double-cat".into();
    //         let content = format!("hello from {psid}\n").into();

    //         client.send(Spawn { psid, command }).await?;
    //         client.send(Input { psid, content }).await?;

    //         Result::<_>::Ok(())
    //     }
    // });

    // for w in future::join_all(work).await {
    //     w?;
    // }

    // time::sleep(Duration::from_millis(333)).await;

    // let work = ids.map(|psid| client.send(Kill { psid }));

    // for w in future::join_all(work).await {
    //     w?;
    // }

    // client.shutdown().await

    let client = Wrap::spawn()?;
    client.full_cmd().await;

    Ok(())
}

struct Client {
    server: process::Child,
    tx_to_server: mpsc::Sender<ServerMessage>,
    tx_to_server_task: task::JoinHandle<Result<()>>,
    rx_from_server_task: task::JoinHandle<Result<()>>,
}

impl Client {
    fn spawn() -> Result<(Self, mpsc::Receiver<ClientMessage>)> {
        let command = "cargo";

        let mut server = Command::new(command)
            .args(["run", "server"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // TODO: How should we report these errors?
            // .stderr(Stdio::piped())
            .spawn()
            .context(UnableToSpawnServerSnafu)?;

        let stdin = server.stdin.take().context(ServerHasNoStdinSnafu)?;
        let stdout = server.stdout.take().context(ServerHasNoStdoutSnafu)?;

        let (tx, rx_from_server) = mpsc::channel(10);
        let rx_from_server_task = task::spawn_blocking(Self::rx_from_server_task(stdout, tx));

        let (tx_to_server, rx) = mpsc::channel(10);
        let tx_to_server_task = task::spawn_blocking(Self::tx_to_server_task(stdin, rx));

        let this = Self {
            server,
            tx_to_server,
            tx_to_server_task,
            rx_from_server_task,
        };

        Ok((this, rx_from_server))
    }

    fn rx_from_server_task(
        stdout: process::ChildStdout,
        tx: mpsc::Sender<ClientMessage>,
    ) -> impl FnOnce() -> Result<()> {
        move || {
            trace!("rx_task booted");
            let stdout = SyncIoBridge::new(stdout);
            let mut stdout = BufReader::new(stdout);

            loop {
                let msg = match rmp_final_read(rmp_serde::decode::from_read(&mut stdout)) {
                    ControlFlow::Continue(msg) => msg,
                    ControlFlow::Break(e) => break e.context(UnableToDecodeMessageFromServerSnafu),
                };

                trace!("got message from the server {msg:?}");

                tx.blocking_send(msg)
                    .context(UnableToRelayMessageFromServerSnafu)?;
            }
        }
    }

    fn tx_to_server_task(
        stdin: process::ChildStdin,
        mut rx: mpsc::Receiver<ServerMessage>,
    ) -> impl FnOnce() -> Result<()> {
        let stdin = SyncIoBridge::new(stdin);
        let mut stdin = BufWriter::new(stdin);

        move || {
            trace!("tx_task booted");
            while let Some(msg) = rx.blocking_recv() {
                trace!("sending message to the server {msg:?}");

                rmp_serde::encode::write(&mut stdin, &msg)
                    .context(UnableToEncodeMessageToServerSnafu)?;
                stdin.flush().context(UnableToFlushMessageToServerSnafu)?;
            }

            Ok(())
        }
    }

    fn handle(&self) -> Handle {
        Handle { tx_to_server: self.tx_to_server.clone() }
    }

    async fn send(&self, cmd: impl Into<ServerMessage>) -> Result<()> {
        self.tx_to_server
            .send(cmd.into())
            .await
            .context(UnableToRelayMessageToServerSnafu)
    }

    async fn shutdown(self) -> Result<()> {
        let Self {
            mut server,
            tx_to_server,
            tx_to_server_task,
            rx_from_server_task,
        } = self;

        drop(tx_to_server);

        let (server, rx_from_server_task, tx_to_server_task) =
            futures::join!(server.wait(), rx_from_server_task, tx_to_server_task,);

        // TODO: Join the errors when there are multiple?

        server.context(UnableToWaitForServerSnafu)?;
        rx_from_server_task.context(UnableToJoinRxFromServerTaskSnafu)??;
        tx_to_server_task.context(UnableToJoinTxToServerTaskSnafu)??;

        Ok(())
    }
}

struct Handle { tx_to_server: mpsc::Sender<ServerMessage> }

impl Handle {
    async fn send(&self, cmd: impl Into<ServerMessage>) -> Result<()> {
        self.tx_to_server
            .send(cmd.into())
            .await
            .context(UnableToRelayMessageToServerSnafu)
    }
}

#[derive(Debug)]
struct Z { spawn: Spawn, doit: oneshot::Sender<(Vec<u8>, Vec<u8>)> }

struct Wrap {
    client: Client,
    z_tx: mpsc::Sender<Z>,
    z_task: task::JoinHandle<Result<()>>,
}

impl Wrap {
    fn spawn() -> Result<Self> {
//        let (tx, rx) = mpsc::channel(10);
        let (client, mut client_rx) = Client::spawn()?;
        let (mut z_tx, mut z_rx) = mpsc::channel(10);

        struct State {
            client: Handle,
            listening: BTreeMap<ProcessSequenceId, (Vec<u8>, Vec<u8>, oneshot::Sender<(Vec<u8>, Vec<u8>)>)>,
        }

        impl State {

            fn new(client: Handle) -> Self {
                Self {
                    client,
                    listening: Default::default(),
                }
            }

            async fn do_z_recv(&mut self, z: Z) {
                trace!(z=?z, "do_z_recv");
                let Z { spawn, doit } = z;
                self.listening.insert(spawn.psid, (Default::default(), Default::default(), doit));
                self.client.send(spawn).await.unwrap();
            }

            fn do_client_recv(&mut self, msg: ClientMessage) {
                use ClientMessage as Msg;

                trace!(msg=?msg, "do_client_recv");

                match &msg {
                    Msg::ChildStarted(..) => {},

                    Msg::Output(Output { psid, stdout, stderr }) => {
                        if let Some(v) = self.listening.get_mut(psid) {
                            v.0.extend_from_slice(stdout);
                            v.1.extend_from_slice(stderr);
                        }
                    }

                    Msg::ChildStopped(ChildStopped { psid }) => {
                        if let Some((stdout, stderr, doit)) = self.listening.remove(psid) {
                            doit.send((stdout, stderr)).unwrap();
                        }
                    }
                }

                // tx.send(msg);

            }
        }

        let z_task = tokio::spawn({
            let client = client.handle();
            async move {
                let mut state = State::new(client);

                loop {
                    tokio::select! {
                        z = z_rx.recv() => {
                            match z {
                                Some(z) => state.do_z_recv(z).await,
                                None => break,
                            }
                        },
                        msg = client_rx.recv() => {
                            match msg {
                                Some(msg) => state.do_client_recv(msg),
                                None => break,
                            }
                        },
                    }
                }

                Ok(())
            }
        });

        let this = Wrap { client, z_tx, z_task };

        Ok(this)
    }

    async fn full_cmd(&self) {
        let psid = ProcessSequenceId::new(42);
        let (tx, rx) = oneshot::channel();
        let spawn = Spawn { psid, command: "uname".into() };
        self.z_tx.send(Z { doit: tx, spawn }).await.unwrap();
        let (stdout, stderr) = rx.await.unwrap();

        let stdout = String::from_utf8_lossy(&stdout);
        let stderr = String::from_utf8_lossy(&stderr);

        eprintln!("{stdout}{stderr}");

    }
}

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("Could not spawn the server process"))]
    UnableToSpawnServer {
        source: std::io::Error,
    },

    #[snafu(display("Server process has no stdin"))]
    ServerHasNoStdin,

    #[snafu(display("Server process has no stdout"))]
    ServerHasNoStdout,

    #[snafu(display("Could not decode message from the server"))]
    UnableToDecodeMessageFromServer {
        source: rmp_serde::decode::Error,
    },

    #[snafu(display("Could not relay message from the server"))]
    UnableToRelayMessageFromServer {
        source: mpsc::error::SendError<ClientMessage>,
    },

    #[snafu(display("Could not encode message to the server"))]
    UnableToEncodeMessageToServer {
        source: rmp_serde::encode::Error,
    },

    #[snafu(display("Could not flush message to the server"))]
    UnableToFlushMessageToServer {
        source: std::io::Error,
    },

    #[snafu(display("Could not relay message to the server"))]
    UnableToRelayMessageToServer {
        source: mpsc::error::SendError<ServerMessage>,
    },

    UnableToWaitForServer {
        source: std::io::Error,
    },

    #[snafu(display(
        "Could not join the task responsible for receiving messages from the server"
    ))]
    UnableToJoinRxFromServerTask {
        source: task::JoinError,
    },

    #[snafu(display("Could not join the task responsible for sending messages to the server"))]
    UnableToJoinTxToServerTask {
        source: task::JoinError,
    },
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
