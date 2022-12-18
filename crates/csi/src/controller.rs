use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    num::TryFromIntError,
};

use tonic::transport::Channel;
use tonic::Code;
use tonic::{Request, Response, Status};
use uuid::Uuid;
use volumed::spec::get_lv_request::Identifier;
use volumed::spec::volume_service_client::VolumeServiceClient;
use volumed::spec::DeleteLvRequest;
use volumed::spec::{CreateLvRequest, Empty, FormatLvRequest, GetLvRequest, LogicalVolume};

use crate::csi::v1_7_0::controller_server::ControllerServer;
use crate::csi::v1_7_0::validate_volume_capabilities_response::Confirmed;
use crate::csi::v1_7_0::volume_capability::{AccessMode, AccessType, BlockVolume, MountVolume};
use crate::csi::v1_7_0::VolumeCapability;
use crate::csi::v1_7_0::{
    controller_server::Controller, list_volumes_response::Entry as VolumeEntry,
    volume_capability::access_mode::Mode, ControllerExpandVolumeRequest,
    ControllerExpandVolumeResponse, ControllerGetCapabilitiesRequest,
    ControllerGetCapabilitiesResponse, ControllerGetVolumeRequest, ControllerGetVolumeResponse,
    ControllerPublishVolumeRequest, ControllerPublishVolumeResponse,
    ControllerUnpublishVolumeRequest, ControllerUnpublishVolumeResponse, CreateSnapshotRequest,
    CreateSnapshotResponse, CreateVolumeRequest, CreateVolumeResponse, DeleteSnapshotRequest,
    DeleteSnapshotResponse, DeleteVolumeRequest, DeleteVolumeResponse, GetCapacityRequest,
    GetCapacityResponse, ListSnapshotsRequest, ListSnapshotsResponse, ListVolumesRequest,
    ListVolumesResponse, Topology, ValidateVolumeCapabilitiesRequest,
    ValidateVolumeCapabilitiesResponse, Volume,
};
use crate::MIN_VOLUME_SIZE_BYTES;

type Client = VolumeServiceClient<Channel>;

#[derive(Clone, Debug)]
pub struct RLVMController {
    node_id: Uuid,
}

impl RLVMController {
    pub fn new(node_id: Uuid) -> Self {
        Self { node_id }
    }

    pub fn into_service(self) -> ControllerServer<Self> {
        ControllerServer::new(self)
    }

    fn get_host_topology(&self) -> Topology {
        Topology {
            segments: HashMap::from([("host".into(), self.node_id.to_string())]),
        }
    }

    /// Returns the accessible topology of the current node
    fn get_access_topologies(&self) -> Vec<Topology> {
        vec![self.get_host_topology()]
    }

    /// Convert a [LogicalVolume] into a [Volume]
    fn process_volume(&self, lv: LogicalVolume) -> Volume {
        Volume {
            capacity_bytes: lv.capacity_bytes as i64,
            volume_id: lv.uuid,
            content_source: None,

            // Attach some LV info for context
            volume_context: HashMap::from([("name".into(), lv.name.to_string())]),

            accessible_topology: self.get_access_topologies(),
        }
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
///   controller_capability!(CreateDeleteVolume),
///   controller_capability!(ListSnapshots),
/// ];
/// ```
macro_rules! controller_capability {
    ( $capability:ident ) => {
        crate::csi::v1_7_0::ControllerServiceCapability {
            r#type: Some(crate::csi::v1_7_0::controller_service_capability::Type::Rpc (
                crate::csi::v1_7_0::controller_service_capability::Rpc {
                    r#type: crate::csi::v1_7_0::controller_service_capability::rpc::Type::$capability.into()
                },
            )),
        }
    };
}

#[tonic::async_trait]
impl Controller for RLVMController {
    async fn list_volumes(
        &self,
        request: Request<ListVolumesRequest>,
    ) -> Result<Response<ListVolumesResponse>, Status> {
        let mut client = request.extensions().get::<Client>().unwrap().clone();
        let req = request.into_inner();

        log::info!("got list volume request with: {:?}", req);

        // Validate the inputs
        let max_entries: usize = req.max_entries.try_into().map_err(|err: TryFromIntError| {
            Status::invalid_argument(format!(
                "`max_entries` must be a valid positive integer: {}",
                err.to_string()
            ))
        })?;

        let start = if !req.starting_token.is_empty() {
            req.starting_token.parse::<usize>().map_err(|err| {
                Status::aborted(format!(
                    "`starting_token` must be a valid positive integer: {}",
                    err.to_string()
                ))
            })?
        } else {
            0
        };

        // Get the LVs from the volumed service
        let lvs: Vec<VolumeEntry> = client
            .get_lv_list(Request::new(Empty {}))
            .await
            .map_err(|err| {
                Status::internal(format!(
                    "could not get_lv_list from volumed: {}",
                    err.to_string()
                ))
            })?
            .into_inner()
            .volumes
            .into_iter()
            .map(|lv| VolumeEntry {
                volume: Some(self.process_volume(lv)),

                // TODO: Qualify the status of the volume using LV attrs
                status: None,
            })
            .take(if max_entries == 0 {
                usize::MAX
            } else {
                max_entries
            })
            .collect();

        let last_index = start + max_entries;
        let length = lvs.len();
        Ok(Response::new(ListVolumesResponse {
            entries: lvs,

            // Only set the `next_token` field if both `max_start` was provided and
            //  if there are more volumes left
            next_token: if last_index < length {
                last_index.to_string()
            } else {
                String::new()
            },
        }))
    }

    // TODO: Does this need to be dumber? As in, should it not care about anything besides
    //  just printing out the capacity of the one tracked drive? For multi controller
    //  setups, they need to converse to ensure that the total capacity is the sum of the
    //  various drives.
    async fn get_capacity(
        &self,
        request: Request<GetCapacityRequest>,
    ) -> Result<Response<GetCapacityResponse>, Status> {
        let mut client = request.extensions().get::<Client>().unwrap().clone();
        let req = request.into_inner();

        // Short out if we have any multinode caps
        let multi_caps = req
            .volume_capabilities
            .into_iter()
            .map(|cap| cap.access_mode.unwrap_or_default().mode())
            .find(|mode| {
                vec![
                    Mode::MultiNodeMultiWriter,
                    Mode::MultiNodeReaderOnly,
                    Mode::MultiNodeSingleWriter,
                ]
                .contains(&mode)
            });

        if multi_caps.is_some() {
            return Ok(Response::new(GetCapacityResponse::default()));
        }

        // Short out if we are asking the capacity of a host other than the current
        if Some(self.get_host_topology()) == req.accessible_topology {
            return Ok(Response::new(GetCapacityResponse::default()));
        }

        // Call out to volumed for the capacity
        let capacity = client
            .get_free_bytes(Empty {})
            .await
            .map_err(|err| {
                Status::internal(format!(
                    "could not get_free_bytes from volumed: {}",
                    err.to_string()
                ))
            })?
            .into_inner()
            .bytes_free;

        let reply = GetCapacityResponse {
            available_capacity: capacity as i64,
            maximum_volume_size: None,
            minimum_volume_size: Some(MIN_VOLUME_SIZE_BYTES as i64),
        };

        Ok(Response::new(reply))
    }

    async fn controller_get_capabilities(
        &self,
        _request: Request<ControllerGetCapabilitiesRequest>,
    ) -> Result<Response<ControllerGetCapabilitiesResponse>, Status> {
        let reply = ControllerGetCapabilitiesResponse {
            capabilities: vec![
                controller_capability!(ListVolumes),
                controller_capability!(CreateDeleteVolume),
                controller_capability!(GetCapacity),
            ],
        };

        Ok(Response::new(reply))
    }

    async fn create_volume(
        &self,
        request: Request<CreateVolumeRequest>,
    ) -> Result<Response<CreateVolumeResponse>, Status> {
        let mut client = request.extensions().get::<Client>().unwrap().clone();
        let req = request.into_inner();

        log::info!("got create volume request: {:?}", req);

        // Validate args
        if req.name.is_empty() {
            return Err(Status::invalid_argument("missing volume name"));
        }
        if req.volume_capabilities.is_empty() {
            return Err(Status::invalid_argument("missing volume capabilities"));
        }

        // Parse the capacity bytes
        // TODO: Do something with the limit
        let (capacity, _limit) = match req.capacity_range {
            Some(cap) => cap
                .required_bytes
                .try_into()
                .map_err(|_| Status::invalid_argument("capacity must be a valid unsigned int"))
                .and_then(|size: usize| {
                    let limit: usize = cap.limit_bytes.try_into().map_err(|_| {
                        Status::invalid_argument("limit bytes must be a valid unsigned int")
                    })?;

                    Ok((size, limit))
                })?,
            None => (MIN_VOLUME_SIZE_BYTES, 0),
        };

        // Call out to volumed for the capacity
        let total_capacity = client
            .get_free_bytes(Empty {})
            .await
            .map_err(|err| {
                Status::internal(format!(
                    "could not get_free_bytes from volumed: {}",
                    err.to_string()
                ))
            })?
            .into_inner()
            .bytes_free;

        // Short out if the capacity is outside the allowable range
        if capacity < MIN_VOLUME_SIZE_BYTES {
            return Err(Status::out_of_range(format!(
                "cannot create a volume smaller than the smallest allowed size: {} < {}",
                capacity, MIN_VOLUME_SIZE_BYTES,
            )));
        }
        if capacity > (total_capacity as usize) {
            return Err(Status::out_of_range(format!(
                "cannot create volume larger than the space available: {} > {}",
                capacity, total_capacity,
            )));
        }

        // Short out if we have already created the volume before
        let safe_name = hash_resource(req.name.clone());
        let volume = client
            .get_logical_volume(Request::new(GetLvRequest {
                identifier: Some(Identifier::Name(safe_name.clone())),
            }))
            .await;

        let volume = match volume {
            Ok(old) => {
                let old = old.into_inner();

                // Fail if the duplicate request has a different size
                if old.capacity_bytes as usize != capacity {
                    return Err(Status::already_exists(format!(
                        "attempting to create an existing volume with different capacities: found {:?}, {} bytes requested",
                        old,
                        capacity,
                    )));
                }

                old
            }
            Err(status) => {
                match status.code() {
                    // Actually create the volume, if not previously found
                    Code::NotFound => client
                        .create_logical_volume(Request::new(CreateLvRequest {
                            name: safe_name.clone(),
                            capacity: capacity as u64,
                            tags: vec![format!("name={}", req.name)],
                        }))
                        .await?
                        .into_inner(),
                    _ => return Err(status),
                }
            }
        };

        // TODO: Only format if we are given a request for an fs volume
        client
            .format_logical_volume(Request::new(FormatLvRequest { name: safe_name }))
            .await?;

        Ok(Response::new(CreateVolumeResponse {
            volume: Some(self.process_volume(volume)),
        }))
    }

    async fn delete_volume(
        &self,
        request: Request<DeleteVolumeRequest>,
    ) -> Result<Response<DeleteVolumeResponse>, Status> {
        let mut client = request.extensions().get::<Client>().unwrap().clone();
        let req = request.into_inner();

        // Validate args
        if req.volume_id.is_empty() {
            return Err(Status::invalid_argument(
                "missing required field `volume_id`",
            ));
        }

        let lv = client
            .get_logical_volume(Request::new(GetLvRequest {
                identifier: Some(Identifier::Uuid(req.volume_id.clone())),
            }))
            .await
            .ok();

        // Delete the volume, if it exists
        if let Some(volume) = lv {
            let volume = volume.into_inner();
            client
                .delete_logical_volume(Request::new(DeleteLvRequest { name: volume.name }))
                .await?;
        } else {
            log::warn!(
                "attempted to delete non-existent volume {}, ignoring...",
                req.volume_id
            );
        }

        Ok(Response::new(DeleteVolumeResponse {}))
    }

    async fn validate_volume_capabilities(
        &self,
        request: Request<ValidateVolumeCapabilitiesRequest>,
    ) -> Result<Response<ValidateVolumeCapabilitiesResponse>, Status> {
        let mut client = request.extensions().get::<Client>().unwrap().clone();
        let req = request.into_inner();

        // Validate args
        if req.volume_id.is_empty() {
            return Err(Status::invalid_argument(
                "missing required field `volume_id`",
            ));
        }
        if req.volume_capabilities.is_empty() {
            return Err(Status::invalid_argument(
                "missing required field `volume_capabilities`",
            ));
        }

        // Fetch the volume in question
        let lv = client
            .get_logical_volume(Request::new(GetLvRequest {
                identifier: Some(Identifier::Uuid(req.volume_id)),
            }))
            .await
            .map_err(|err| Status::not_found(err.to_string()))?
            .into_inner();

        // TODO: We need to check the specific capabilities passed by the CO...
        let reply = ValidateVolumeCapabilitiesResponse {
            confirmed: Some(Confirmed {
                parameters: HashMap::new(),
                volume_capabilities: vec![
                    VolumeCapability {
                        access_mode: Some(AccessMode {
                            mode: Mode::SingleNodeWriter.into(),
                        }),
                        access_type: Some(AccessType::Block(BlockVolume {}).into()),
                    },
                    VolumeCapability {
                        access_mode: Some(AccessMode {
                            mode: Mode::SingleNodeWriter.into(),
                        }),
                        access_type: Some(
                            AccessType::Mount(MountVolume {
                                fs_type: "xfs".into(),
                                mount_flags: vec![],

                                // TODO: No idea what this is
                                volume_mount_group: "".into(),
                            })
                            .into(),
                        ),
                    },
                ],

                volume_context: HashMap::from([("name".into(), lv.name.to_string())]),
            }),
            message: "".into(),
        };

        Ok(Response::new(reply))
    }

    // --- Unimplemented below ---

    async fn controller_publish_volume(
        &self,
        _request: Request<ControllerPublishVolumeRequest>,
    ) -> Result<Response<ControllerPublishVolumeResponse>, Status> {
        Err(Status::unimplemented("not implemented"))
    }

    async fn controller_unpublish_volume(
        &self,
        _request: Request<ControllerUnpublishVolumeRequest>,
    ) -> Result<Response<ControllerUnpublishVolumeResponse>, Status> {
        Err(Status::unimplemented("not implemented"))
    }

    async fn create_snapshot(
        &self,
        _request: Request<CreateSnapshotRequest>,
    ) -> Result<Response<CreateSnapshotResponse>, Status> {
        Err(Status::unimplemented("not implemented"))
    }

    async fn delete_snapshot(
        &self,
        _request: Request<DeleteSnapshotRequest>,
    ) -> Result<Response<DeleteSnapshotResponse>, Status> {
        Err(Status::unimplemented("not implemented"))
    }

    async fn list_snapshots(
        &self,
        _request: Request<ListSnapshotsRequest>,
    ) -> Result<Response<ListSnapshotsResponse>, Status> {
        Err(Status::unimplemented("not implemented"))
    }

    async fn controller_expand_volume(
        &self,
        _request: Request<ControllerExpandVolumeRequest>,
    ) -> Result<Response<ControllerExpandVolumeResponse>, Status> {
        Err(Status::unimplemented("not implemented"))
    }

    async fn controller_get_volume(
        &self,
        _request: Request<ControllerGetVolumeRequest>,
    ) -> Result<Response<ControllerGetVolumeResponse>, Status> {
        Err(Status::unimplemented("not implemented"))
    }
}

/// Hashes a resource for safe usage
pub(crate) fn hash_resource<T>(obj: T) -> String
where
    T: Hash,
{
    let mut hasher = DefaultHasher::new();
    obj.hash(&mut hasher);

    format!("{:X}", hasher.finish())
}
