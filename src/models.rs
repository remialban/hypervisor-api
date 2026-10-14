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

/// Body of `POST .../vms`. What can be chosen is listed by `GET .../vm-options`.
#[derive(Deserialize, ToSchema)]
pub struct CreateVm {
    pub name: String,
    pub memory_mb: u64,
    pub vcpus: u64,
    /// At least one disk, unless an ISO is given.
    #[serde(default)]
    pub disks: Vec<DiskSpec>,
    /// Installation medium, attached as a CD-ROM and booted first.
    pub iso: Option<VolumeRef>,
    /// Virtual network to plug the VM into (default: `default`).
    pub network: Option<String>,
    /// `bios` (default) or `uefi`.
    pub firmware: Option<String>,
    /// Start the VM right after its creation (default: false).
    pub start: Option<bool>,
}

/// A disk of the new VM: either a new volume (`size_gb`) or an existing one (`volume`).
#[derive(Deserialize, ToSchema)]
pub struct DiskSpec {
    /// Storage pool holding (or receiving) the volume.
    pub pool: String,
    /// Creates a new thin-provisioned qcow2 volume of this size.
    pub size_gb: Option<u64>,
    /// Attaches this existing volume, which must not be used by another VM.
    pub volume: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct VolumeRef {
    pub pool: String,
    pub volume: String,
}

// Not stored in SQLite: built from libvirt, so no FromRow.
#[derive(Deserialize, ToSchema)]
pub struct CloneVm {
    pub name: String,
}

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

#[derive(Clone, Serialize, ToSchema, FromRow)]
pub struct Disk {
    pub id: String,
    pub hypervisor_id: String,
    pub name: String,
    pub size_gb: i64,
    pub format: String,
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
