use snafu::prelude::*;
use std::ops::ControlFlow;
use tracing_subscriber::prelude::*;

mod client;
mod messages;
mod server;

/// Run the providence command
#[derive(argh::FromArgs)]
struct Config {
    #[argh(subcommand)]
    command: Command,
}

#[derive(argh::FromArgs)]
#[argh(subcommand)]
enum Command {
    Server(server::Config),
    Client(client::Config),
}

#[tokio::main]
#[snafu::report]
async fn main() -> Result<()> {
    let config: Config = argh::from_env();

    tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(tracing_subscriber::EnvFilter::from_env("PROVIDENCE_LOG"))
        .with_writer(std::io::stderr)
        .finish()
        .init();

    match config.command {
        Command::Server(cfg) => server::main(cfg).await?,
        Command::Client(cfg) => client::main(cfg).await?,
    }

    Ok(())
}

#[derive(Debug, Snafu)]
#[allow(clippy::enum_variant_names)]
enum Error {
    #[snafu(display("Could not set the tracing subscriber"))]
    #[snafu(context(false))]
    UnableToSetTracingSubscriber {
        source: tracing::subscriber::SetGlobalDefaultError,
    },

    #[snafu(display("Could not execute the server"))]
    #[snafu(context(false))]
    UnableToExecuteServer { source: server::Error },

    #[snafu(display("Could not execute the client"))]
    #[snafu(context(false))]
    UnableToExecuteClient { source: client::Error },
}

type Result<T, E = Error> = std::result::Result<T, E>;

fn rmp_final_read<T>(r: DecodeResult<T>) -> ControlFlow<DecodeResult<()>, T> {
    match r {
        Ok(msg) => ControlFlow::Continue(msg),
        Err(rmp_serde::decode::Error::InvalidMarkerRead(e))
            if e.kind() == std::io::ErrorKind::UnexpectedEof =>
        {
            ControlFlow::Break(Ok(()))
        }
        Err(e) => ControlFlow::Break(Err(e)),
    }
}

type DecodeResult<T> = std::result::Result<T, rmp_serde::decode::Error>;
