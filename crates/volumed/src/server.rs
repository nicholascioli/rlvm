use std::num::TryFromIntError;

use lvm2_cmd::{
    error::LVMError,
    lv::{LVCreateOptions, LogicalVolume},
    vg::VolumeGroup,
    InvalidResourceCapacityError, InvalidResourceNameError, InvalidResourceUUIDError,
    ResourceSelector,
};
use tonic::{Request, Response, Status};

use crate::{
    spec::{
        get_lv_request::Identifier,
        volume_service_server::{VolumeService, VolumeServiceServer},
        CreateLvRequest, DeleteLvRequest, Empty, FormatLvRequest, GetFreeBytesResponse,
        GetLvListResponse, GetLvRequest, LogicalVolume as LV,
    },
    Config,
};

pub struct VolumedServer {
    config: Config,
}

impl VolumedServer {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub fn into_service(self) -> VolumeServiceServer<Self> {
        VolumeServiceServer::new(self)
    }
}

#[tonic::async_trait]
impl VolumeService for VolumedServer {
    async fn get_lv_list(
        &self,
        request: Request<Empty>,
    ) -> Result<Response<GetLvListResponse>, Status> {
        let vg = request.extensions().get::<VolumeGroup>().unwrap();

        let reply = vg
            .list_lvs()
            .map_err(map_lvm_error)?
            .into_iter()
            .map(LogicalVolume::into)
            .collect();

        Ok(Response::new(GetLvListResponse { volumes: reply }))
    }

    async fn get_free_bytes(
        &self,
        request: Request<Empty>,
    ) -> Result<Response<GetFreeBytesResponse>, Status> {
        let vg = request.extensions().get::<VolumeGroup>().unwrap();
        let spare_bytes = self.config.spare_bytes.unwrap_or_default();

        Ok(Response::new(GetFreeBytesResponse {
            bytes_free: vg
                .capacity_bytes
                .checked_sub(spare_bytes)
                .unwrap_or(0)
                .try_into()
                .map_err(|err: TryFromIntError| {
                    Status::internal(format!(
                        "could not cast capacity into a u64: {}",
                        err.to_string()
                    ))
                })?,
        }))
    }

    async fn create_logical_volume(
        &self,
        request: Request<CreateLvRequest>,
    ) -> Result<Response<LV>, Status> {
        let vg = request.extensions().get::<VolumeGroup>().unwrap().clone();
        let req = request.into_inner();

        let capacity = req
            .capacity
            .try_into()
            .map_err(|err: InvalidResourceCapacityError| {
                Status::invalid_argument(err.to_string())
            })?;

        let name = req
            .name
            .try_into()
            .map_err(|err: InvalidResourceNameError| Status::invalid_argument(err.to_string()))?;

        let lv = vg
            .add_lv(LVCreateOptions {
                activate: true,
                capacity_bytes: capacity,
                name: name,
                tags: req.tags,
            })
            .map_err(map_lvm_error)?;

        Ok(Response::new(lv.into()))
    }

    async fn delete_logical_volume(
        &self,
        request: Request<DeleteLvRequest>,
    ) -> Result<Response<Empty>, Status> {
        let vg = request.extensions().get::<VolumeGroup>().unwrap().clone();
        let req = request.into_inner();

        let name = req
            .name
            .try_into()
            .map_err(|err: InvalidResourceNameError| Status::invalid_argument(err.to_string()))?;

        vg.remove_lv(&name).map_err(map_lvm_error)?;

        Ok(Response::new(Empty {}))
    }

    async fn format_logical_volume(
        &self,
        request: Request<FormatLvRequest>,
    ) -> Result<Response<Empty>, Status> {
        let vg = request.extensions().get::<VolumeGroup>().unwrap().clone();
        let req = request.into_inner();

        // Get the resource equivalents of the names
        let name = req
            .name
            .try_into()
            .map_err(|err: InvalidResourceNameError| Status::invalid_argument(err.to_string()))?;

        // Get the LV
        let lv = LogicalVolume::from_id(&vg.name, &name).map_err(map_lvm_error)?;

        // Format the volume
        let cmd = std::process::Command::new("mkfs.xfs")
            .arg("-f")
            .arg(&lv.path)
            .output()
            .map_err(|err| {
                Status::internal(format!(
                    "could not run mkfs.xfs command: {}",
                    err.to_string()
                ))
            })?;

        // Print out the stderr if the command failed
        if !cmd.status.success() {
            return Err(Status::internal(format!(
                "could not format volume `{}`: {}",
                name,
                String::from_utf8_lossy(&cmd.stderr)
            )));
        }

        Ok(Response::new(Empty {}))
    }

    async fn get_logical_volume(
        &self,
        request: Request<GetLvRequest>,
    ) -> Result<Response<LV>, Status> {
        let vg = request.extensions().get::<VolumeGroup>().unwrap().clone();
        let req = request.into_inner();

        let id = req.identifier.ok_or(Status::invalid_argument(
            "missing required field `identifier`",
        ))?;

        let lv = match id {
            Identifier::Uuid(uuid) => {
                let uuid = uuid.try_into().map_err(|err: InvalidResourceUUIDError| {
                    Status::invalid_argument(err.to_string())
                })?;

                LogicalVolume::from_uuid(&uuid).map_err(map_lvm_error)?
            }
            Identifier::Name(name) => {
                let name = name.try_into().map_err(|err: InvalidResourceNameError| {
                    Status::invalid_argument(err.to_string())
                })?;

                LogicalVolume::from_id(&vg.name, &name).map_err(map_lvm_error)?
            }
        };

        Ok(Response::new(lv.into()))
    }
}

impl From<LogicalVolume> for LV {
    fn from(lv: LogicalVolume) -> Self {
        LV {
            uuid: lv.uuid.to_string(),
            name: lv.name.to_string(),
            // TODO: What do we do if the capacity is larger than a u64?
            capacity_bytes: (*lv.capacity_bytes).try_into().unwrap_or_default(),
            volume_group: lv.volume_group_name.to_string(),
        }
    }
}

/// Maps an LVM error into its equivalent status code
#[inline]
fn map_lvm_error(err: LVMError) -> Status {
    match err {
        LVMError::NotFound { .. } => Status::not_found(err.to_string()),
        LVMError::Command { .. } => Status::invalid_argument(err.to_string()),
        _ => Status::internal(err.to_string()),
    }
}
