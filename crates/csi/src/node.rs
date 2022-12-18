use std::{collections::HashMap, path::Path};

use mountd::spec::{
    mount_service_client::MountServiceClient, GetLvmBlockPathRequest, Mount, MountFlag,
    MountRequest, UnmountRequest,
};
use tonic::{transport::Channel, Request, Response, Status};
use uuid::Uuid;

use crate::csi::v1_7_0::{
    node_server::{Node, NodeServer},
    volume_capability::access_mode::Mode,
    NodeGetCapabilitiesRequest, NodeGetCapabilitiesResponse, NodeGetInfoRequest,
    NodeGetInfoResponse, NodePublishVolumeRequest, NodePublishVolumeResponse,
    NodeStageVolumeRequest, NodeStageVolumeResponse, NodeUnpublishVolumeRequest,
    NodeUnpublishVolumeResponse, NodeUnstageVolumeRequest, NodeUnstageVolumeResponse, Topology,
};

type Client = MountServiceClient<Channel>;

#[derive(Debug)]
pub struct RLVMNode {
    node_id: Uuid,
}

impl RLVMNode {
    /// Create a node which tracks the specified volume groups
    pub fn new(node_id: Uuid) -> Self {
        RLVMNode { node_id }
    }

    /// Convert the controller into an intercepted service
    // TODO: Can this be moved into a trait?
    pub fn into_service(self) -> NodeServer<Self> {
        NodeServer::new(self)
    }
}

/// Construct the needed structure for a controller capability.
///
/// Takes the capability type ([crate::csi::v1_7_0::controller_capability::Type]) and
/// variant.
///
/// # Examples
///
/// ```no_run
/// let capabilities = vec! [
///   node_capability!(CreateDeleteVolume),
///   node_capability!(ListSnapshots),
/// ];
/// ```
macro_rules! node_capability {
    ( $capability:ident ) => {
        crate::csi::v1_7_0::NodeServiceCapability {
            r#type: Some(crate::csi::v1_7_0::node_service_capability::Type::Rpc(
                crate::csi::v1_7_0::node_service_capability::Rpc {
                    r#type: crate::csi::v1_7_0::node_service_capability::rpc::Type::$capability
                        .into(),
                },
            )),
        }
    };
}

#[tonic::async_trait]
impl Node for RLVMNode {
    async fn node_stage_volume(
        &self,
        request: Request<NodeStageVolumeRequest>,
    ) -> Result<Response<NodeStageVolumeResponse>, Status> {
        let mut client = request.extensions().get::<Client>().unwrap().clone();
        let req = request.into_inner();

        log::info!("got NodeStageVolume request: {:?}", req);

        // Validate args
        if req.volume_id.is_empty() {
            return Err(Status::invalid_argument("`volume_id` cannot be empty"));
        }
        if req.staging_target_path.is_empty() {
            return Err(Status::invalid_argument(
                "`staging_target_path` cannot be empty",
            ));
        }
        if req.volume_capability.is_none() {
            return Err(Status::invalid_argument(
                "`volume_capability` cannot be empty",
            ));
        }

        // Attempt to get the device matching the volume ID from the mountd service
        let block_device = client
            .get_lvm_block_path(Request::new(GetLvmBlockPathRequest {
                uuid: req.volume_id.clone(),
            }))
            .await?
            .into_inner();
        let mount_src = Path::new(&block_device.path);
        let mount_dst = Path::new(&req.staging_target_path);

        // Make sure that the mountpoint exists
        if !mount_src.exists() {
            return Err(Status::failed_precondition(format!(
                "volume with id `{}` does not have a valid mount path: is it active?",
                req.volume_id
            )));
        }

        if !mount_dst.exists() {
            return Err(Status::failed_precondition(format!(
                "volume with id `{}` does not have a valid mount destination: {}",
                req.volume_id, req.staging_target_path,
            )));
        }

        // Generate flags as needed
        let readonly = req
            .volume_capability
            .and_then(|cap| cap.access_mode)
            .map(|access_mode| access_mode.mode == Mode::SingleNodeReaderOnly as i32)
            .unwrap_or_default();

        // Mount to the staging path
        client
            .mount(Request::new(MountRequest {
                mount: Some(Mount {
                    src: mount_src.to_string_lossy().to_string(),
                    dst: mount_dst.to_string_lossy().to_string(),
                }),
                flags: if readonly {
                    vec![MountFlag::ReadOnly.into()]
                } else {
                    vec![]
                },
            }))
            .await
            .map(|_| Response::new(NodeStageVolumeResponse {}))
    }

    async fn node_unstage_volume(
        &self,
        request: Request<NodeUnstageVolumeRequest>,
    ) -> Result<Response<NodeUnstageVolumeResponse>, Status> {
        let mut client = request.extensions().get::<Client>().unwrap().clone();
        let req = request.into_inner();

        log::info!("got NodeUnstageVolume request: {:?}", req);

        // Validate args
        if req.volume_id.is_empty() {
            return Err(Status::invalid_argument("`volume_id` cannot be empty"));
        }
        if req.staging_target_path.is_empty() {
            return Err(Status::invalid_argument(
                "`staging_target_path` cannot be empty",
            ));
        }

        // Attempt to get the device matching the volume ID from the mountd service
        let block_device = client
            .get_lvm_block_path(Request::new(GetLvmBlockPathRequest {
                uuid: req.volume_id.clone(),
            }))
            .await?
            .into_inner();
        let unmount_src = std::path::Path::new(&block_device.path);

        // Make sure that the mountpoint exists
        if !unmount_src.exists() {
            return Err(Status::failed_precondition(format!(
                "volume with id `{}` does not have a valid mount path: is it active?",
                req.volume_id
            )));
        }

        // Unmount to the staging path
        client
            .unmount(Request::new(UnmountRequest {
                path: req.staging_target_path,
            }))
            .await
            .map(|_| Response::new(NodeUnstageVolumeResponse {}))
    }

    async fn node_publish_volume(
        &self,
        request: Request<NodePublishVolumeRequest>,
    ) -> Result<Response<NodePublishVolumeResponse>, Status> {
        let mut client = request.extensions().get::<Client>().unwrap().clone();
        let req = request.into_inner();

        log::info!("got NodePublish request: {:?}", req);

        // Validate args
        if req.volume_id.is_empty() {
            return Err(Status::invalid_argument("`volume_id` cannot be empty"));
        }
        if req.staging_target_path.is_empty() {
            return Err(Status::invalid_argument(
                "`staging_target_path` cannot be empty",
            ));
        }
        if req.volume_capability.is_none() {
            return Err(Status::invalid_argument(
                "`volume_capability` cannot be empty",
            ));
        }

        // Verify that the volume ID is valid
        client
            .get_lvm_block_path(Request::new(GetLvmBlockPathRequest {
                uuid: req.volume_id.clone(),
            }))
            .await?
            .into_inner();

        let mount_src = Path::new(&req.staging_target_path);
        let mount_dst = Path::new(&req.target_path);

        // It is our responsibility to create this path...
        tokio::fs::create_dir_all(&mount_dst).await.map_err(|err| {
            Status::internal(format!("could not create target path: {}", err.to_string()))
        })?;

        // Make sure that the mountpoint exists
        if !mount_src.exists() {
            return Err(Status::failed_precondition(format!(
                "volume with id `{}` does not have a valid mount path: was it staged?",
                req.volume_id
            )));
        }

        // Generate flags as needed
        let readonly = req
            .volume_capability
            .and_then(|cap| cap.access_mode)
            .map(|access_mode| access_mode.mode == Mode::SingleNodeReaderOnly as i32)
            .unwrap_or_default();

        // Mount to the staging path
        client
            .mount(Request::new(MountRequest {
                mount: Some(Mount {
                    src: mount_src.to_string_lossy().to_string(),
                    dst: mount_dst.to_string_lossy().to_string(),
                }),

                // TODO: Is there no way to conditionally have elements?
                flags: [
                    vec![MountFlag::Bind.into()],
                    if readonly {
                        vec![MountFlag::ReadOnly.into()]
                    } else {
                        vec![]
                    },
                ]
                .concat(),
            }))
            .await
            .map(|_| Response::new(NodePublishVolumeResponse {}))
    }

    async fn node_unpublish_volume(
        &self,
        request: Request<NodeUnpublishVolumeRequest>,
    ) -> Result<Response<NodeUnpublishVolumeResponse>, Status> {
        let mut client = request.extensions().get::<Client>().unwrap().clone();
        let req = request.into_inner();

        log::info!("got NodeUnpublish request: {:?}", req);

        // Validate args
        if req.volume_id.is_empty() {
            return Err(Status::invalid_argument("`volume_id` cannot be empty"));
        }
        if req.target_path.is_empty() {
            return Err(Status::invalid_argument("`target_path` cannot be empty"));
        }

        // Verify that the volume ID is valid
        client
            .get_lvm_block_path(Request::new(GetLvmBlockPathRequest {
                uuid: req.volume_id.clone(),
            }))
            .await?
            .into_inner();

        let unmount_src = std::path::Path::new(&req.target_path);

        // Unmount to the staging path
        client
            .unmount(Request::new(UnmountRequest {
                path: unmount_src.to_string_lossy().into(),
            }))
            .await?;

        // It is our responsibility to delete this path...
        tokio::fs::remove_dir(&unmount_src).await.ok();

        Ok(Response::new(NodeUnpublishVolumeResponse {}))
    }

    fn node_get_volume_stats<'life0, 'async_trait>(
        &'life0 self,
        _request: tonic::Request<crate::csi::v1_7_0::NodeGetVolumeStatsRequest>,
    ) -> core::pin::Pin<
        Box<
            dyn core::future::Future<
                    Output = Result<
                        tonic::Response<crate::csi::v1_7_0::NodeGetVolumeStatsResponse>,
                        tonic::Status,
                    >,
                > + core::marker::Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        todo!()
    }

    fn node_expand_volume<'life0, 'async_trait>(
        &'life0 self,
        _request: tonic::Request<crate::csi::v1_7_0::NodeExpandVolumeRequest>,
    ) -> core::pin::Pin<
        Box<
            dyn core::future::Future<
                    Output = Result<
                        tonic::Response<crate::csi::v1_7_0::NodeExpandVolumeResponse>,
                        tonic::Status,
                    >,
                > + core::marker::Send
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        todo!()
    }

    async fn node_get_capabilities(
        &self,
        _request: Request<NodeGetCapabilitiesRequest>,
    ) -> Result<Response<NodeGetCapabilitiesResponse>, Status> {
        let reply = NodeGetCapabilitiesResponse {
            capabilities: vec![node_capability!(StageUnstageVolume)],
        };

        Ok(Response::new(reply))
    }

    async fn node_get_info(
        &self,
        _request: Request<NodeGetInfoRequest>,
    ) -> Result<Response<NodeGetInfoResponse>, Status> {
        let reply = NodeGetInfoResponse {
            accessible_topology: Some(Topology {
                segments: HashMap::from([("host".into(), self.node_id.to_string())]),
            }),

            // TODO: Do we want this to be some multiple of the smallest allowed size?
            max_volumes_per_node: 0,
            node_id: self.node_id.to_string(),
        };

        Ok(Response::new(reply))
    }
}
