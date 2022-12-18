use std::path::Path;

use lvm2_cmd::{error::LVMError, lv::LogicalVolume, InvalidResourceUUIDError, ResourceSelector};
use mountpoints::mountpaths;
use nix::unistd::chown;
use sys_mount::{unmount, Mount, MountFlags, UnmountFlags};
use tonic::{Request, Response, Status};

use crate::{
    spec::{
        mount_service_server::{MountService, MountServiceServer},
        BlockDevice, GetLvmBlockPathRequest,
        MountFlag::{self, ReadOnly},
        MountRequest, MountResponse, UnmountRequest, UnmountResponse,
    },
    Config,
};

pub struct MountdServer {
    config: Config,
}

impl MountdServer {
    pub fn new(cfg: Config) -> Self {
        Self { config: cfg }
    }

    pub fn into_service(self) -> MountServiceServer<Self> {
        MountServiceServer::new(self)
    }
}

#[tonic::async_trait]
impl MountService for MountdServer {
    async fn get_lvm_block_path(
        &self,
        request: Request<GetLvmBlockPathRequest>,
    ) -> Result<Response<BlockDevice>, Status> {
        let req = request.into_inner();
        let uuid = req
            .uuid
            .try_into()
            .map_err(|err: InvalidResourceUUIDError| Status::invalid_argument(err.to_string()))?;

        let lv = LogicalVolume::from_uuid(&uuid).map_err(|err| match err {
            LVMError::NotFound { .. } => Status::not_found(err.to_string()),
            _ => Status::internal(err.to_string()),
        })?;

        Ok(Response::new(BlockDevice {
            path: lv.path.to_string_lossy().to_string(),
        }))
    }

    async fn mount(
        &self,
        request: Request<MountRequest>,
    ) -> Result<Response<MountResponse>, Status> {
        let req = request.into_inner();
        let mounts = mountpaths().map_err(|err| {
            Status::internal(format!("could not get mountpoints: {}", err.to_string()))
        })?;

        log::info!("got mount request: {:?}", req);

        let mount = req
            .mount
            .ok_or(Status::invalid_argument("missing required `mount` arg"))?;

        // Verify that we got a dev / dest
        if mount.src.is_empty() {
            return Err(Status::invalid_argument(
                "missing required field `src` in mount",
            ));
        }
        if mount.dst.is_empty() {
            return Err(Status::invalid_argument(
                "missing required field `dst` in mount",
            ));
        }

        // Make sure that both paths are allowed
        let src = Path::new(&mount.src);
        let dst = Path::new(&mount.dst);

        self.config.ensure_interactable(src, false).map_err(|err| {
            Status::permission_denied(format!(
                "specified source `{}` cannot be mounted by current config: {}",
                src.to_string_lossy(),
                err.to_string()
            ))
        })?;
        self.config
            .ensure_interactable(dst, req.flags.contains(&ReadOnly.into()))
            .map_err(|err| {
                Status::permission_denied(format!(
                    "specified destination `{}` cannot be mounted by current config: {}",
                    dst.to_string_lossy(),
                    err.to_string()
                ))
            })?;

        // Also make sure that the destination is a directory
        if !dst.is_dir() {
            return Err(Status::failed_precondition(format!(
                "mount dst is not a directory: {}",
                dst.to_string_lossy(),
            )));
        }

        // Short out if the endpoint is already mounted
        if mounts.contains(&dst.into()) {
            log::info!(
                "skipping specified mountpoint, as it is already mounted: {}",
                dst.to_string_lossy()
            );
            return Ok(Response::new(MountResponse {}));
        }

        // Gather the mount flags
        let mapped: Result<Vec<_>, _> = req
            .flags
            .into_iter()
            .map(|flag| {
                MountFlag::from_i32(flag).ok_or(Status::invalid_argument("invalid mount flag"))
            })
            .collect();

        let mut flags = MountFlags::from_iter(mapped?.into_iter().map(MountFlag::into));

        // Always apply a few options for security
        // NODEV means that any nested block devices will not be mounted
        // NOSUID means that any SUID executable will be mounted without the SUID flag
        flags.insert(MountFlags::from_iter([
            MountFlags::NODEV,
            MountFlags::NOSUID,
        ]));

        // Mount the request
        let result = Mount::builder()
            .fstype("xfs")
            .flags(flags)
            .mount(src, dst)
            .map_err(|err| {
                Status::internal(format!("could not mount request: {}", err.to_string()))
            })?;

        log::info!("mounted request with flags {:?}: {:?}", flags, result);

        // Own the mounted folder for the specified user / group
        let (uid, gid) = self.config.get_owner_pair();
        chown(dst, Some(uid), Some(gid)).map_err(|err| {
            Status::internal(format!(
                "could not chown mount for specified user ({}:{}): {}",
                uid,
                gid,
                err.to_string()
            ))
        })?;

        Ok(Response::new(MountResponse {}))
    }

    async fn unmount(
        &self,
        request: Request<UnmountRequest>,
    ) -> Result<Response<UnmountResponse>, Status> {
        let req = request.into_inner();
        let mounts = mountpaths().map_err(|err| {
            Status::internal(format!("could not get mountpoints: {}", err.to_string()))
        })?;

        log::info!("got unmount request: {:?}", req);

        // Verify that we got a path
        if req.path.is_empty() {
            return Err(Status::invalid_argument(
                "missing required field `path` in unmount",
            ));
        }

        let mountpoint = Path::new(&req.path);

        // Short out if the path is not mounted
        if !mounts.contains(&mountpoint.into()) {
            log::info!(
                "skipping specified mountpoint, as it is not mounted: {}",
                mountpoint.to_string_lossy()
            );

            return Ok(Response::new(UnmountResponse {}));
        }

        // Make sure that we can interact with the endpoint
        self.config
            .ensure_interactable(mountpoint, false)
            .map_err(|err| {
                Status::permission_denied(format!(
                    "specified mountpoint `{}` cannot be unmounted by current config: {}",
                    mountpoint.to_string_lossy(),
                    err.to_string()
                ))
            })?;

        // Actually unmount
        unmount(mountpoint, UnmountFlags::empty()).map_err(|err| {
            Status::internal(format!("could not unmount endpoint: {}", err.to_string()))
        })?;

        log::info!("unmounted request: {}", mountpoint.to_string_lossy());

        Ok(Response::new(UnmountResponse {}))
    }
}

impl From<MountFlag> for MountFlags {
    fn from(flag: MountFlag) -> Self {
        match flag {
            MountFlag::Unknown => MountFlags::empty(),
            MountFlag::Bind => MountFlags::BIND,
            MountFlag::ReadOnly => MountFlags::RDONLY,
        }
    }
}
