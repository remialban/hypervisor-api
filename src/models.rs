use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
pub struct Message {
    pub status: String,
    pub content: String,
}

#[derive(Clone, Serialize, ToSchema, FromRow)]
pub struct Hypervisor {
    pub id: String,
    pub name: String,
    pub uri: String,
    pub status: String,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateHypervisor {
    pub name: String,
    pub uri: String,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateHypervisor {
    pub name: Option<String>,
    pub uri: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateVm {
    pub name: String,
    pub memory_mb: u64,
    pub vcpus: u16,
}

// Not stored in SQLite: built from libvirt, so no FromRow.
#[derive(Clone, Serialize, ToSchema)]
pub struct VirtualMachine {
    pub id: String,
    pub hypervisor_id: String,
    pub name: String,
    pub memory_mb: i64,
    pub vcpus: i64,
    pub status: String,
}

#[derive(Deserialize, ToSchema)]
pub struct DiskRequest {
    pub name: String,
    pub size_gb: u64,
    pub format: String,
}

#[derive(Deserialize, ToSchema)]
pub struct DiskUpdateRequest {
    pub size_gb: u64,
}

#[derive(Clone, Serialize, ToSchema, FromRow)]
pub struct Disk {
    pub id: String,
    pub hypervisor_id: String,
    pub name: String,
    pub size_gb: i64,
    pub format: String,
}

#[derive(Clone, Serialize, ToSchema)]
pub struct StoragePool {
    pub id: String,
    pub hypervisor_id: String,
    pub name: String,
    pub active: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct StoragePoolRequest {
    pub name: String,
    pub target_path: String,
}

#[derive(Clone, Serialize, ToSchema, FromRow)]
pub struct Network {
    pub id: String,
    pub hypervisor_id: String,
    pub name: String,
    pub bridge: Option<String>,
    pub status: String,
}

#[derive(Deserialize, ToSchema)]
pub struct NetworkRequest {
    pub name: String,
    pub bridge: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct VncResponse {
    pub hypervisor_id: String,
    pub vm_id: String,
    pub uuid: String,
    pub host: String,
    pub port: String,
    pub token: String,
    pub novnc_url: String,
    pub status: String,
}
