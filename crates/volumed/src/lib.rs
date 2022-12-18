use serde::Deserialize;

pub mod server;

pub mod spec {
    tonic::include_proto!("volumed");
}

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    /// The [VolumeGroup] to manage
    pub volume_group: String,

    /// The optional amount of bytes to reserve free
    pub spare_bytes: Option<usize>,
}
