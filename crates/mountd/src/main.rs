use std::path::PathBuf;

use clap::Parser;
use futures_util::FutureExt;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

use mountd::{server::MountdServer, Config};

#[derive(Parser)]
struct Cli {
    /// Path to the config file
    #[clap(default_value = "/etc/mountd/mountd.yaml")]
    config: PathBuf,

    /// Path to the listening socket
    #[clap(short, long, default_value = "/run/mountd/mountd.sock")]
    socket_path: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize the env_logger
    env_logger::init();

    // Parse the CLI options
    let args = Cli::parse();

    // Attempt to parse the config file
    let cfg_file = std::fs::File::open(&args.config).map_err(|err| {
        format!(
            "could not open config file {}: {}",
            args.config.to_string_lossy(),
            err.to_string()
        )
    })?;

    let cfg: Config = serde_yaml::from_reader(cfg_file).map_err(|err| {
        format!(
            "invalid config at {}: {}",
            args.config.to_string_lossy(),
            err.to_string()
        )
    })?;

    log::info!("Found config: {:?}", cfg);

    // Create the unix socket for communication
    let sock = UnixListener::bind(&args.socket_path)?;
    let sock_stream = UnixListenerStream::new(sock);

    // Set up the server
    log::info!(
        "Starting the mountd service at `{}`",
        args.socket_path.to_string_lossy()
    );

    let controller = MountdServer::new(cfg);

    // Handle SIGINT cleanly by cleaning up the socket when killed
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    ctrlc::set_handler(move || tx.blocking_send(()).expect("could not send sigint"))
        .expect("could not set Ctrl-C handler");

    // Start listening
    Server::builder()
        .add_service(controller.into_service())
        // Serve until we get a Ctrl^C (or are killed)
        .serve_with_incoming_shutdown(sock_stream, rx.recv().map(|_| ()))
        .await?;

    // Clean up the socket file
    log::info!("Cleaning up socket file...");
    tokio::fs::remove_file(&args.socket_path).await?;

    Ok(())
}
