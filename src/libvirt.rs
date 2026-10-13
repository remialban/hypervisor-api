use sqlx::SqlitePool;
use std::collections::HashMap;
use uuid::Uuid;

use virt::{
    connect::Connect, domain::Domain, error::ErrorNumber, storage_pool::StoragePool,
    storage_vol::StorageVol, sys,
};

use crate::{errors::ApiError, models::VirtualMachine, vm_xml};

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
