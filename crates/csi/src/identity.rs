use std::collections::HashMap;

use mountd::spec::mount_service_client::MountServiceClient;
use tonic::{transport::Channel, Request, Response, Status};
use volumed::spec::{volume_service_client::VolumeServiceClient, Empty};

use crate::csi::v1_7_0::{
    identity_server::{Identity, IdentityServer},
    GetPluginCapabilitiesRequest, GetPluginCapabilitiesResponse, GetPluginInfoRequest,
    GetPluginInfoResponse, ProbeRequest, ProbeResponse,
};

#[derive(Clone, Debug)]
pub enum Verifier {
    Controller,
    Node,
}

impl Verifier {
    pub async fn verify(&self, request: Request<ProbeRequest>) -> Option<bool> {
        match self {
            Self::Controller => {
                let mut client = request
                    .extensions()
                    .get::<VolumeServiceClient<Channel>>()
                    .expect("could not get volumed client")
                    .clone();

                client
                    .get_free_bytes(Request::new(Empty::default()))
                    .await
                    .map(|_| true)
                    .ok()
            }
            Self::Node => {
                let mut _client = request
                    .extensions()
                    .get::<MountServiceClient<Channel>>()
                    .expect("could not get mountd client")
                    .clone();

                // TODO
                Some(true)
            }
        }
    }
}

#[derive(Debug)]
pub struct RLVMIdentity {
    verifier: Verifier,
}

impl RLVMIdentity {
    pub fn new(verifier: Verifier) -> Self {
        Self { verifier }
    }

    /// Convert the identity into an intercepted service
    // TODO: Can this be moved into a trait?
    pub fn into_service(self) -> IdentityServer<Self> {
        IdentityServer::new(self)
    }
}

/// Construct the needed structure for a plugin capability.
///
/// Takes the capability type ([crate::csi::v1_7_0::plugin_capability::Type]) and
/// variant.
///
/// # Examples
///
/// ```no_run
/// let capabilities = vec! [
///   plugin_capability!(Service, ControllerService),
///   plugin_capability!(VolumeExpansion, Online),
/// ];
/// ```
macro_rules! plugin_capability {
    ( $type:ident, $variant:ident ) => {
        ::paste::paste! {
            crate::csi::v1_7_0::PluginCapability {
                r#type: Some(crate::csi::v1_7_0::plugin_capability::Type::$type (
                    crate::csi::v1_7_0::plugin_capability::$type {
                        r#type: crate::csi::v1_7_0::plugin_capability::[<$type:snake>]::Type::$variant.into()
                    },
                )),
            }
        }
    };
}

#[tonic::async_trait]
impl Identity for RLVMIdentity {
    async fn get_plugin_info(
        &self,
        _request: Request<GetPluginInfoRequest>,
    ) -> Result<Response<GetPluginInfoResponse>, Status> {
        let reply = GetPluginInfoResponse {
            name: "org.github.rlvm".into(),
            vendor_version: "0.1.0".into(),
            manifest: HashMap::new(),
        };

        Ok(Response::new(reply))
    }

    async fn get_plugin_capabilities(
        &self,
        _request: Request<GetPluginCapabilitiesRequest>,
    ) -> Result<Response<GetPluginCapabilitiesResponse>, Status> {
        let reply = GetPluginCapabilitiesResponse {
            capabilities: vec![
                plugin_capability!(Service, ControllerService),
                plugin_capability!(Service, VolumeAccessibilityConstraints),
                // plugin_capability!(VolumeExpansion, Online),
                // plugin_capability!(VolumeExpansion, Offline),
            ],
        };

        Ok(Response::new(reply))
    }

    async fn probe(
        &self,
        request: Request<ProbeRequest>,
    ) -> Result<Response<ProbeResponse>, Status> {
        let reply = ProbeResponse {
            ready: self.verifier.verify(request).await,
        };

        Ok(Response::new(reply))
    }
}
