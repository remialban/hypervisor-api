//! What a client may choose when creating a VM on a given hypervisor.
//!
//! The limits and defaults below are the single source of truth: they are returned to
//! the UI by `GET .../vm-options` and are the ones `POST .../vms` validates against.

use axum::{
    Json,
    extract::{Path, State},
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::{errors::ApiError, libvirt, models::Message, state::SharedState};

pub const MIN_MEMORY_MB: u64 = 64;
pub const MAX_MEMORY_MB: u64 = 1_048_576;
pub const MIN_VCPUS: u64 = 1;
pub const MAX_VCPUS: u64 = 256;
pub const MIN_DISK_GB: u64 = 1;
pub const MAX_DISK_GB: u64 = 2048;

pub const DEFAULT_NETWORK: &str = "default";
pub const DEFAULT_FIRMWARE: &str = "bios";
pub const DEFAULT_MEMORY_MB: u64 = 2048;
pub const DEFAULT_VCPUS: u64 = 2;
pub const DEFAULT_DISK_GB: u64 = 20;

pub const FIRMWARES: [&str; 2] = ["bios", "uefi"];

/// A pool with more volumes than this is cut, and flagged as `truncated`.
pub const MAX_VOLUMES_PER_POOL: usize = 500;

#[derive(Serialize, ToSchema)]
pub struct VmOptions {
    /// Active storage pools: where new disks can be created.
    pub storage_pools: Vec<PoolOption>,
    /// Active virtual networks a VM can be plugged into.
    pub networks: Vec<NetworkOption>,
    pub firmware: Vec<String>,
    pub limits: Limits,
    pub defaults: Defaults,
}

#[derive(Serialize, ToSchema)]
pub struct PoolOption {
    pub name: String,
    pub capacity_gb: u64,
    /// Indicative only: disks are thin-provisioned, creation does not check it.
    pub available_gb: u64,
    pub volumes: Vec<VolumeOption>,
    /// True when the pool holds more volumes than are listed.
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum VolumeKind {
    Disk,
    Iso,
}

#[derive(Serialize, ToSchema)]
pub struct VolumeOption {
    pub name: String,
    pub format: String,
    pub size_mb: u64,
    pub kind: VolumeKind,
    /// Names of the VMs already using this volume as a disk (always empty for ISOs).
    /// A disk that is already used should not be offered for a new VM.
    pub used_by: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct NetworkOption {
    pub name: String,
    pub bridge: Option<String>,
    /// `nat`, `route`, `bridge`... or null for an isolated network.
    pub forward_mode: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct Range {
    pub min: u64,
    pub max: u64,
}

#[derive(Serialize, ToSchema)]
pub struct Limits {
    pub memory_mb: Range,
    pub vcpus: Range,
    pub disk_gb: Range,
}

#[derive(Serialize, ToSchema)]
pub struct Defaults {
    pub network: String,
    pub firmware: String,
    pub memory_mb: u64,
    pub vcpus: u64,
    pub disk_gb: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_mb: Range {
                min: MIN_MEMORY_MB,
                max: MAX_MEMORY_MB,
            },
            vcpus: Range {
                min: MIN_VCPUS,
                max: MAX_VCPUS,
            },
            disk_gb: Range {
                min: MIN_DISK_GB,
                max: MAX_DISK_GB,
            },
        }
    }
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            network: DEFAULT_NETWORK.into(),
            firmware: DEFAULT_FIRMWARE.into(),
            memory_mb: DEFAULT_MEMORY_MB,
            vcpus: DEFAULT_VCPUS,
            disk_gb: DEFAULT_DISK_GB,
        }
    }
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/vm-options", params(("hypervisor_id" = String, Path)), responses((status = 200, body = VmOptions), (status = 404, body = Message), (status = 502, body = Message)), tag = "VMs")]
pub async fn get(
    State(state): State<SharedState>,
    Path(hypervisor_id): Path<String>,
) -> Result<Json<VmOptions>, ApiError> {
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    Ok(Json(libvirt::vm_options(uri).await?))
}
