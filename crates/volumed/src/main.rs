use std::path::PathBuf;

use clap::Parser;
use futures_util::FutureExt;
use lvm2_cmd::{vg::VolumeGroup, InvalidResourceNameError, ResourceName};
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;

use tonic::{transport::Server, Request, Status};
use volumed::{server::VolumedServer, Config};

#[derive(Parser)]
struct Cli {
    /// Path to the config file
    #[clap(default_value = "/etc/volumed/volumed.yaml")]
    config: PathBuf,

    /// Path to the listening socket
    #[clap(short, long, default_value = "/run/volumed/volumed.sock")]
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

    // Ensure that we can see the volume group
    let resource = cfg
        .volume_group
        .clone()
        .try_into()
        .map_err(|err: InvalidResourceNameError| err.to_string())?;
    let vg = VolumeGroup::from_id(&resource).map_err(|err| {
        format!(
            "could not find specified volume group `{}`: {}",
            &resource,
            err.to_string()
        )
    })?;

    log::info!("managing volume group `{}`: {:?}", resource, vg);

    // Ensure that the spare_bytes aren't larger than the capacity
    if let Some(spare_bytes) = &cfg.spare_bytes {
        if *vg.capacity_bytes <= *spare_bytes {
            return Err(format!(
                "capacity of managed volume ({}) is not larger than the requested spare_bytes ({})",
                vg.capacity_bytes, spare_bytes
            )
            .into());
        }
    }

    // Create the unix socket for communication
    let sock = UnixListener::bind(&args.socket_path)?;
    let sock_stream = UnixListenerStream::new(sock);

    // Set up the server
    log::info!(
        "Starting the volumed service at `{}`",
        args.socket_path.to_string_lossy()
    );

    let controller = VolumedServer::new(cfg.clone());

    // Handle SIGINT cleanly by cleaning up the socket when killed
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    ctrlc::set_handler(move || tx.blocking_send(()).expect("could not send sigint"))
        .expect("could not set Ctrl-C handler");

    // Start listening
    Server::builder()
        // Intercept all requests and log them
        .layer(tonic::service::interceptor(vg_injector(cfg, resource)))
        .add_service(controller.into_service())
        // Serve until we get a Ctrl^C (or are killed)
        .serve_with_incoming_shutdown(sock_stream, rx.recv().map(|_| ()))
        .await?;

    // Clean up the socket file
    log::info!("Cleaning up socket file...");
    tokio::fs::remove_file(&args.socket_path).await?;

    Ok(())
}

/// Intercept a request and append the [VolumeGroup] info to it.
// TODO: This should probably be consumed by the library...
fn vg_injector(
    config: Config,
    resource: ResourceName,
) -> impl Fn(Request<()>) -> Result<Request<()>, Status> + Send + Clone {
    move |mut req| {
        let info = VolumeGroup::from_id(&resource).map_err(|_| {
            Status::internal(format!("volume group not found: {}", &config.volume_group))
        })?;

        // Inject the volume group into the request
        req.extensions_mut().insert(info);

        Ok(req)
    }
}
