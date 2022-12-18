pub mod controller;
pub mod identity;
pub mod node;

pub mod csi {
    pub mod v1_7_0 {
        tonic::include_proto!("csi.v1");
    }
}

/// Allow for a minimum volume size of 512M (must be multiple of 512)
pub const MIN_VOLUME_SIZE_BYTES: usize = 512 * 1024 * 1024;
