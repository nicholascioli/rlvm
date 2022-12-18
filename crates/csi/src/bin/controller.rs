use std::path::PathBuf;

use clap::Parser;
use futures_util::FutureExt;
use tokio::net::{UnixListener, UnixStream};
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{
    transport::{Endpoint, Server, Uri},
    Request, Status,
};
use tower::service_fn;
use uuid::Uuid;
use volumed::spec::volume_service_client::VolumeServiceClient;

use rlvm::{
    controller::RLVMController,
    identity::{RLVMIdentity, Verifier},
};

#[derive(Debug, Parser)]
struct Cli {
    /// Unique ID for this node
    #[clap(short, long, default_value_t = Uuid::new_v4())]
    node_id: Uuid,

    /// Path to the listening socket
    #[clap(short, long, default_value = "/run/rlvm/controller.sock")]
    socket_path: PathBuf,

    /// Path to the volumed socket
    volumed_socket: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize the env_logger
    env_logger::init();

    // Parse the CLI options
    let args = Cli::parse();

    // Create the unix socket for communication
    let sock = UnixListener::bind(&args.socket_path)?;
    let sock_stream = UnixListenerStream::new(sock);

    // Set up the server
    log::info!(
        "Starting the rlvm controller service at `{}`",
        args.socket_path.to_string_lossy()
    );

    let controller = RLVMController::new(args.node_id);
    let identity = RLVMIdentity::new(Verifier::Controller);

    // Handle SIGINT cleanly by cleaning up the socket when killed
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    ctrlc::set_handler(move || tx.blocking_send(()).expect("could not send sigint"))
        .expect("could not set Ctrl-C handler");

    // Start listening
    Server::builder()
        .layer(tonic::service::interceptor(
            client_injector(args.volumed_socket).await,
        ))
        .add_service(controller.into_service())
        .add_service(identity.into_service())
        // Serve until we get a Ctrl^C (or are killed)
        .serve_with_incoming_shutdown(sock_stream, rx.recv().map(|_| ()))
        .await?;

    // Clean up the socket file
    log::info!("Cleaning up socket file...");
    tokio::fs::remove_file(&args.socket_path).await?;

    Ok(())
}

async fn client_injector(
    socket: PathBuf,
) -> impl Fn(Request<()>) -> Result<Request<()>, Status> + Send + Clone {
    let channel = Endpoint::try_from("lttp://[::]:50051")
        .expect("super internal error")
        .connect_with_connector(service_fn(move |_: Uri| {
            UnixStream::connect(socket.to_owned())
        }))
        .await
        .expect("could not connect to volumed socket");

    // Create a client for the volumed service
    let client = VolumeServiceClient::new(channel);

    move |mut req| {
        // Inject the client into the request
        req.extensions_mut().insert(client.clone());

        Ok(req)
    }
}
