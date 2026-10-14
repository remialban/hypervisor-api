use sqlx::SqlitePool;
use std::collections::HashMap;
use uuid::Uuid;

use virt::{
    connect::Connect, domain::Domain, error::ErrorNumber, network::Network,
    storage_pool::StoragePool, storage_vol::StorageVol, sys,
};

use crate::{
    errors::ApiError,
    models::{CreateVm, VirtualMachine},
    vm_create,
    vm_options::{
        self, Defaults, Limits, NetworkOption, PoolOption, VmOptions, VolumeKind, VolumeOption,
    },
    vm_xml,
};

/// Reads the libvirt URI of a hypervisor from SQLite.
pub async fn hypervisor_uri(db: &SqlitePool, hypervisor_id: &str) -> Result<String, ApiError> {
    sqlx::query_scalar::<_, String>("SELECT uri FROM hypervisors WHERE id = ?")
        .bind(hypervisor_id)
        .fetch_optional(db)
        .await
        .map_err(|e| {
            eprintln!("SQLite read failed: {e}");
            ApiError::Internal("SQLite read failed".into())
        })?
        .ok_or_else(|| ApiError::NotFound("Hypervisor not found".into()))
}

/// Lists every VM (running or not) known by the hypervisor.
pub async fn list_vms(uri: String, hypervisor_id: String) -> Result<Vec<VirtualMachine>, ApiError> {
    run_blocking(move || {
        let conn = Connect::open(Some(&uri))?;
        // 0 = no filter: active and inactive domains.
        let domains = conn.list_all_domains(0)?;
        let mut vms = Vec::with_capacity(domains.len());
        for domain in &domains {
            match to_vm(domain, &hypervisor_id) {
                Ok(vm) => vms.push(vm),
                // Deleted while we were listing: it is simply no longer there.
                Err(ApiError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(vms)
    })
    .await
}

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;

/// Everything a client may choose when creating a VM: pools, volumes, networks, limits.
pub async fn vm_options(uri: String) -> Result<VmOptions, ApiError> {
    run_blocking(move || {
        let conn = Connect::open(Some(&uri))?;
        let used_by = disks_in_use(&conn)?;

        let mut storage_pools = Vec::new();
        for pool in conn.list_all_storage_pools(sys::VIR_CONNECT_LIST_STORAGE_POOLS_ACTIVE)? {
            // A pool removed while we read it is simply skipped.
            match pool_option(&pool, &used_by) {
                Ok(option) => storage_pools.push(option),
                Err(ApiError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        storage_pools.sort_by(|a, b| a.name.cmp(&b.name));

        let mut networks = Vec::new();
        for network in conn.list_all_networks(sys::VIR_CONNECT_LIST_NETWORKS_ACTIVE)? {
            networks.push(NetworkOption {
                name: network.get_name()?,
                bridge: network.get_bridge_name().ok(),
                forward_mode: vm_xml::network_forward_mode(&network.get_xml_desc(0)?)?,
            });
        }
        networks.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(VmOptions {
            storage_pools,
            networks,
            firmware: vm_options::FIRMWARES
                .iter()
                .map(|f| f.to_string())
                .collect(),
            limits: Limits::default(),
            defaults: Defaults::default(),
        })
    })
    .await
}

/// Disk file path -> names of the VMs using it.
fn disks_in_use(conn: &Connect) -> Result<HashMap<String, Vec<String>>, ApiError> {
    let mut used: HashMap<String, Vec<String>> = HashMap::new();
    for domain in conn.list_all_domains(0)? {
        let (name, xml) = match (domain.get_name(), domain.get_xml_desc(0)) {
            (Ok(name), Ok(xml)) => (name, xml),
            (Err(e), _) | (_, Err(e)) if e.code() == ErrorNumber::NoDomain => continue,
            (Err(e), _) | (_, Err(e)) => return Err(e.into()),
        };
        for path in vm_xml::disk_sources(&xml)? {
            used.entry(path).or_default().push(name.clone());
        }
    }
    Ok(used)
}

fn pool_option(
    pool: &StoragePool,
    used_by: &HashMap<String, Vec<String>>,
) -> Result<PoolOption, ApiError> {
    let info = pool.get_info()?;
    let volumes = pool.list_all_volumes(0)?;
    let truncated = volumes.len() > vm_options::MAX_VOLUMES_PER_POOL;

    let mut options = Vec::new();
    for volume in volumes.iter().take(vm_options::MAX_VOLUMES_PER_POOL) {
        match volume_option(volume, used_by) {
            Ok(Some(option)) => options.push(option),
            // Directories, or a volume deleted while we read the pool.
            Ok(None) => {}
            Err(ApiError::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
    }
    options.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(PoolOption {
        name: pool.get_name()?,
        capacity_gb: info.capacity / GIB,
        available_gb: info.available / GIB,
        volumes: options,
        truncated,
    })
}

/// `None` for entries that are neither a disk image nor an ISO (directories, archives...).
fn volume_option(
    volume: &StorageVol,
    used_by: &HashMap<String, Vec<String>>,
) -> Result<Option<VolumeOption>, ApiError> {
    let info = volume.get_info().map_err(volume_error)?;
    if info.kind != sys::VIR_STORAGE_VOL_FILE && info.kind != sys::VIR_STORAGE_VOL_BLOCK {
        return Ok(None);
    }
    let name = volume.get_name().map_err(volume_error)?;
    let format = vm_xml::volume_format(&volume.get_xml_desc(0).map_err(volume_error)?)?;
    let is_block = info.kind == sys::VIR_STORAGE_VOL_BLOCK;
    let Some(kind) = vm_options::classify_volume(&name, &format, is_block) else {
        return Ok(None);
    };
    let path = volume.get_path().map_err(volume_error)?;
    Ok(Some(VolumeOption {
        name,
        format,
        size_mb: info.capacity / MIB,
        kind,
        used_by: if kind == VolumeKind::Iso {
            Vec::new()
        } else {
            used_by.get(&path).cloned().unwrap_or_default()
        },
    }))
}

/// A volume that disappeared is reported as NotFound so callers can skip it.
fn volume_error(e: virt::error::Error) -> ApiError {
    if e.code() == ErrorNumber::NoStorageVolume {
        ApiError::NotFound("Volume not found".into())
    } else {
        e.into()
    }
}

/// Where a disk of the new VM comes from, resolved and checked before anything is created.
enum DiskPlan {
    New {
        pool: StoragePool,
        volume: String,
        size_gb: u64,
    },
    Existing {
        path: String,
        format: String,
    },
}

struct CreationPlan {
    disks: Vec<DiskPlan>,
    iso_path: Option<String>,
    network: String,
    kvm: bool,
}

/// Creates a VM (and the new disks it asks for) from a validated request. The VM is left
/// stopped unless `start` is set. Nothing is left behind if the creation fails.
pub async fn create_vm(
    uri: String,
    hypervisor_id: String,
    request: CreateVm,
    vnc_listen: String,
) -> Result<VirtualMachine, ApiError> {
    vm_create::validate(&request)?;
    run_blocking(move || {
        let conn = Connect::open(Some(&uri))?;
        let plan = plan_creation(&conn, &request)?;

        // Remember every volume we create, to delete them if the definition fails.
        let mut created: Vec<StorageVol> = Vec::new();
        let domain =
            match create_volumes_and_define(&conn, &request, plan, &vnc_listen, &mut created) {
                Ok(domain) => domain,
                Err(error) => {
                    for volume in &created {
                        if let Err(e) = volume.delete(0) {
                            eprintln!("Could not remove volume after a failed creation: {e}");
                        }
                    }
                    return Err(error);
                }
            };

        if request.start.unwrap_or(false)
            && let Err(e) = domain.create()
        {
            eprintln!("Could not start the new VM: {e}");
            return Err(ApiError::Internal(
                "The VM was created but could not be started".into(),
            ));
        }
        to_vm(&domain, &hypervisor_id)
    })
    .await
}

/// Checks everything the request depends on, without changing anything.
fn plan_creation(conn: &Connect, request: &CreateVm) -> Result<CreationPlan, ApiError> {
    if Domain::lookup_by_name(conn, &request.name).is_ok() {
        return Err(ApiError::Conflict(
            "A VM with this name already exists".into(),
        ));
    }
    let network_name = request
        .network
        .clone()
        .unwrap_or_else(|| vm_options::DEFAULT_NETWORK.to_string());
    if !Network::lookup_by_name(conn, &network_name)?.is_active()? {
        return Err(ApiError::Conflict("The network is not active".into()));
    }

    let in_use = disks_in_use(conn)?;
    let mut disks = Vec::with_capacity(request.disks.len());
    for (index, disk) in request.disks.iter().enumerate() {
        let pool = active_pool(conn, &disk.pool)?;
        match (disk.size_gb, &disk.volume) {
            (Some(size_gb), _) => {
                let volume = if index == 0 {
                    format!("{}.qcow2", request.name)
                } else {
                    format!("{}-disk{index}.qcow2", request.name)
                };
                if StorageVol::lookup_by_name(&pool, &volume).is_ok() {
                    return Err(ApiError::Conflict(format!(
                        "A volume named {volume} already exists in pool {}",
                        disk.pool
                    )));
                }
                disks.push(DiskPlan::New {
                    pool,
                    volume,
                    size_gb,
                });
            }
            (None, Some(name)) => {
                let (volume, format) = find_volume(&pool, name, VolumeKind::Disk)?;
                let path = volume.get_path()?;
                if in_use.contains_key(&path) {
                    return Err(ApiError::Conflict(format!(
                        "The disk {name} is already used by a VM"
                    )));
                }
                disks.push(DiskPlan::Existing { path, format });
            }
            (None, None) => {
                return Err(ApiError::BadRequest(
                    "A disk needs a size or a volume".into(),
                ));
            }
        }
    }

    let iso_path = match &request.iso {
        Some(iso) => {
            let pool = active_pool(conn, &iso.pool)?;
            let (volume, _) = find_volume(&pool, &iso.volume, VolumeKind::Iso)?;
            Some(volume.get_path()?)
        }
        None => None,
    };

    Ok(CreationPlan {
        disks,
        iso_path,
        network: network_name,
        kvm: conn.get_type()? == "QEMU",
    })
}

fn active_pool(conn: &Connect, name: &str) -> Result<StoragePool, ApiError> {
    let pool = StoragePool::lookup_by_name(conn, name)?;
    if !pool.is_active()? {
        return Err(ApiError::Conflict(format!(
            "The storage pool {name} is not active"
        )));
    }
    Ok(pool)
}

/// Finds a volume and checks it is the expected kind (disk image or ISO).
fn find_volume(
    pool: &StoragePool,
    name: &str,
    expected: VolumeKind,
) -> Result<(StorageVol, String), ApiError> {
    let volume = StorageVol::lookup_by_name(pool, name)?;
    let info = volume.get_info()?;
    let format = vm_xml::volume_format(&volume.get_xml_desc(0)?)?;
    let is_block = info.kind == sys::VIR_STORAGE_VOL_BLOCK;
    if vm_options::classify_volume(name, &format, is_block) != Some(expected) {
        let wanted = if expected == VolumeKind::Iso {
            "an ISO"
        } else {
            "a disk image"
        };
        return Err(ApiError::BadRequest(format!(
            "The volume {name} is not {wanted}"
        )));
    }
    Ok((volume, format))
}

fn create_volumes_and_define(
    conn: &Connect,
    request: &CreateVm,
    plan: CreationPlan,
    vnc_listen: &str,
    created: &mut Vec<StorageVol>,
) -> Result<Domain, ApiError> {
    let mut attachments = Vec::with_capacity(plan.disks.len());
    for disk in plan.disks {
        match disk {
            DiskPlan::New {
                pool,
                volume,
                size_gb,
            } => {
                let xml = vm_xml::new_disk_volume_xml(&volume, size_gb)?;
                let new_volume = StorageVol::create_xml(&pool, &xml, 0)?;
                let path = new_volume.get_path()?;
                created.push(new_volume);
                attachments.push(vm_xml::DiskAttachment {
                    path,
                    format: "qcow2".into(),
                });
            }
            DiskPlan::Existing { path, format } => {
                attachments.push(vm_xml::DiskAttachment { path, format });
            }
        }
    }
    let spec = vm_xml::DomainSpec {
        name: request.name.clone(),
        memory_mb: request.memory_mb,
        vcpus: request.vcpus,
        kvm: plan.kvm,
        uefi: request.firmware.as_deref() == Some("uefi"),
        disks: attachments,
        iso_path: plan.iso_path,
        network: plan.network,
        vnc_listen: vnc_listen.to_string(),
    };
    Ok(Domain::define_xml(conn, &vm_xml::new_domain_xml(&spec)?)?)
}

/// Reads one VM from its libvirt UUID.
pub async fn get_vm(
    uri: String,
    hypervisor_id: String,
    vm_id: String,
) -> Result<VirtualMachine, ApiError> {
    with_domain(uri, vm_id, move |domain| to_vm(domain, &hypervisor_id)).await
}

/// Starts a stopped VM and returns its new state.
pub async fn start_vm(
    uri: String,
    hypervisor_id: String,
    vm_id: String,
) -> Result<VirtualMachine, ApiError> {
    with_domain(uri, vm_id, move |domain| {
        let (state, _) = domain.get_state()?;
        if state != sys::VIR_DOMAIN_SHUTOFF && state != sys::VIR_DOMAIN_CRASHED {
            return Err(ApiError::Conflict("The VM is not stopped".into()));
        }
        domain.create()?;
        to_vm(domain, &hypervisor_id)
    })
    .await
}

/// Stops a VM. `force = false` sends an ACPI shutdown request: the guest decides when
/// to stop, so the returned state may still be `running`. `force = true` cuts the power.
pub async fn stop_vm(
    uri: String,
    hypervisor_id: String,
    vm_id: String,
    force: bool,
) -> Result<VirtualMachine, ApiError> {
    with_domain(uri, vm_id, move |domain| {
        let (state, _) = domain.get_state()?;
        if state == sys::VIR_DOMAIN_SHUTOFF {
            return Err(ApiError::Conflict("The VM is already stopped".into()));
        }
        if force {
            domain.destroy()?;
        } else {
            domain.shutdown()?;
        }
        to_vm(domain, &hypervisor_id)
    })
    .await
}

/// Deletes a stopped VM (its definition and NVRAM). With `delete_disks`, also deletes its
/// disk volumes, unless one is shared with another VM or is not in a storage pool: in that
/// case nothing is deleted.
pub async fn delete_vm(uri: String, vm_id: String, delete_disks: bool) -> Result<(), ApiError> {
    with_connection_domain(uri, vm_id, move |conn, domain| {
        let (state, _) = domain.get_state()?;
        if state != sys::VIR_DOMAIN_SHUTOFF {
            return Err(ApiError::Conflict(
                "The VM must be stopped before deletion".into(),
            ));
        }

        // Check everything before deleting anything.
        let mut volumes = Vec::new();
        if delete_disks {
            let paths = vm_xml::disk_sources(&domain.get_xml_desc(0)?)?;
            ensure_not_shared(conn, domain, &paths)?;
            for path in &paths {
                let volume = StorageVol::lookup_by_path(conn, path).map_err(|_| {
                    ApiError::Conflict("A disk of the VM is not in a libvirt storage pool".into())
                })?;
                volumes.push(volume);
            }
        }

        // libvirt refuses to undefine a VM that has an NVRAM file unless asked to remove it.
        if vm_xml::has_nvram(&domain.get_xml_desc(0)?)? {
            domain.undefine_flags(sys::VIR_DOMAIN_UNDEFINE_NVRAM)?;
        } else {
            domain.undefine()?;
        }

        for volume in &volumes {
            volume.delete(0).map_err(|e| {
                eprintln!("Could not delete a disk after removing the VM: {e}");
                ApiError::Internal("The VM was deleted but a disk could not be removed".into())
            })?;
        }
        Ok(())
    })
    .await
}

/// Fails if another VM uses one of `paths`.
fn ensure_not_shared(conn: &Connect, domain: &Domain, paths: &[String]) -> Result<(), ApiError> {
    let own_id = domain.get_uuid_string()?;
    for other in conn.list_all_domains(0)? {
        // A VM deleted while we scan the others cannot be using the disk any more.
        let xml = match (other.get_uuid_string(), other.get_xml_desc(0)) {
            (Ok(id), _) if id == own_id => continue,
            (Ok(_), Ok(xml)) => xml,
            (Err(e), _) | (_, Err(e)) if e.code() == ErrorNumber::NoDomain => continue,
            (Err(e), _) | (_, Err(e)) => return Err(e.into()),
        };
        let used = vm_xml::disk_sources(&xml)?;
        if paths.iter().any(|p| used.contains(p)) {
            return Err(ApiError::Conflict(
                "A disk of the VM is also used by another VM".into(),
            ));
        }
    }
    Ok(())
}

/// Clones a stopped VM: copies its disks into the same storage pools, then defines a new
/// (stopped) VM using the copies. Blocking and slow: it copies the whole disks.
pub async fn clone_vm(
    uri: String,
    hypervisor_id: String,
    vm_id: String,
    new_name: String,
) -> Result<VirtualMachine, ApiError> {
    vm_xml::validate_name(&new_name)?;
    with_connection_domain(uri, vm_id, move |conn, source| {
        let (state, _) = source.get_state()?;
        if state != sys::VIR_DOMAIN_SHUTOFF {
            return Err(ApiError::Conflict(
                "The source VM must be stopped before cloning".into(),
            ));
        }
        if Domain::lookup_by_name(conn, &new_name).is_ok() {
            return Err(ApiError::Conflict(
                "A VM with this name already exists".into(),
            ));
        }
        let xml = source.get_xml_desc(0)?;

        // Remember every volume we create, to delete them if a later step fails.
        let mut created: Vec<StorageVol> = Vec::new();
        let result = copy_disks_and_define(conn, &xml, &new_name, &mut created)
            .and_then(|domain| to_vm(&domain, &hypervisor_id));
        if result.is_err() {
            for volume in &created {
                if let Err(e) = volume.delete(0) {
                    eprintln!("Could not remove volume after a failed clone: {e}");
                }
            }
        }
        result
    })
    .await
}

fn copy_disks_and_define(
    conn: &Connect,
    source_xml: &str,
    new_name: &str,
    created: &mut Vec<StorageVol>,
) -> Result<Domain, ApiError> {
    let mut copies = HashMap::new();
    for (index, path) in vm_xml::disk_sources(source_xml)?.iter().enumerate() {
        let source_volume = StorageVol::lookup_by_path(conn, path).map_err(|_| {
            ApiError::Conflict("A disk of the source VM is not in a libvirt storage pool".into())
        })?;
        let pool = StoragePool::lookup_by_volume(&source_volume)?;
        let suffix = if index == 0 {
            String::new()
        } else {
            format!("-disk{index}")
        };
        let volume_xml = vm_xml::new_volume_xml(
            &source_volume.get_xml_desc(0)?,
            &format!("{new_name}{suffix}.qcow2"),
        )?;
        let copy = StorageVol::create_xml_from(&pool, &volume_xml, &source_volume, 0)?;
        copies.insert(path.clone(), copy.get_path()?);
        created.push(copy);
    }
    let definition = vm_xml::clone_definition(source_xml, new_name, &copies)?;
    Ok(Domain::define_xml(conn, &definition)?)
}

/// Opens a connection, finds the domain from its UUID and runs `action` on it.
async fn with_domain<T, F>(uri: String, vm_id: String, action: F) -> Result<T, ApiError>
where
    F: FnOnce(&Domain) -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    with_connection_domain(uri, vm_id, move |_, domain| action(domain)).await
}

/// Same as `with_domain`, but the action also gets the connection.
async fn with_connection_domain<T, F>(uri: String, vm_id: String, action: F) -> Result<T, ApiError>
where
    F: FnOnce(&Connect, &Domain) -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    // Also protects the C call below: a UUID with a NUL byte would make it panic.
    Uuid::parse_str(&vm_id).map_err(|_| ApiError::BadRequest("Invalid VM identifier".into()))?;
    run_blocking(move || {
        let conn = Connect::open(Some(&uri))?;
        let domain = Domain::lookup_by_uuid_string(&conn, &vm_id)?;
        action(&conn, &domain)
    })
    .await
}

/// libvirt is blocking: run it on a dedicated thread so the async runtime is not stalled.
async fn run_blocking<T, F>(job: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|e| ApiError::Internal(format!("Blocking task failed: {e}")))?
}

fn to_vm(domain: &Domain, hypervisor_id: &str) -> Result<VirtualMachine, ApiError> {
    let info = domain.get_info()?;
    Ok(VirtualMachine {
        id: domain.get_uuid_string()?,
        hypervisor_id: hypervisor_id.to_string(),
        name: domain.get_name()?,
        // libvirt reports KiB.
        memory_mb: (info.max_mem / 1024) as i64,
        vcpus: info.nr_virt_cpu as i64,
        status: state_label(info.state).to_string(),
    })
}

fn state_label(state: sys::virDomainState) -> &'static str {
    match state {
        sys::VIR_DOMAIN_RUNNING | sys::VIR_DOMAIN_BLOCKED => "running",
        sys::VIR_DOMAIN_PAUSED | sys::VIR_DOMAIN_PMSUSPENDED => "paused",
        sys::VIR_DOMAIN_SHUTDOWN => "shutting_down",
        sys::VIR_DOMAIN_SHUTOFF => "stopped",
        sys::VIR_DOMAIN_CRASHED => "crashed",
        _ => "unknown",
    }
}
