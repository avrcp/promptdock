#![forbid(unsafe_code)]

pub mod protocol;
mod run;

pub use protocol::{
    CatalogQueryActionV5, CatalogQueryResultV5, ControlActionV5, ControlResultV5,
    GatewayCapabilityV5, GetDeviceInfoResultV5, HarnessProfileCatalogResultV5,
    HarnessProfileDescriptorV5, RuntimeCapabilityFacetV5, RuntimeCatalogResultV5,
    RuntimeDescriptorV5, StartRunResultV5, TaskPresetCatalogResultV5, TaskPresetDescriptorV5,
    WorkspaceCatalogResultV5, WorkspaceDescriptorV5, WorkspaceSensitivityV5,
};
pub use run::{
    RemoteDesiredStateV5, RemoteRunConditionV5, RemoteRunDetailV5, RemoteRunFilterV5,
    RemoteRunOutcomeV5, RemoteRunPageV5, RemoteRunPhaseV5, RemoteRunSummaryV5, RemoteRunTreeNodeV5,
    RemoteRunTreeV5, RunQueryActionV5, RunQueryResultV5, RunQueryValidationError,
};
