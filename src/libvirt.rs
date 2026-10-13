use sqlx::SqlitePool;
use uuid::Uuid;
use virt::{connect::Connect, domain::Domain, sys};

use crate::{errors::ApiError, models::VirtualMachine};

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
        domains
            .iter()
            .map(|domain| to_vm(domain, &hypervisor_id))
            .collect()
    })
    .await
}

/// Reads one VM from its libvirt UUID.
pub async fn get_vm(uri: String, hypervisor_id: String, vm_id: String) -> Result<VirtualMachine, ApiError> {
    with_domain(uri, vm_id, move |domain| to_vm(domain, &hypervisor_id)).await
}

/// Starts a stopped VM and returns its new state.
pub async fn start_vm(uri: String, hypervisor_id: String, vm_id: String) -> Result<VirtualMachine, ApiError> {
    with_domain(uri, vm_id, move |domain| {
        domain.create()?;
        to_vm(domain, &hypervisor_id)
    })
    .await
}

/// Stops a VM. `force = false` sends an ACPI shutdown request: the guest decides when
/// to stop, so the returned state may still be `running`. `force = true` cuts the power.
pub async fn stop_vm(uri: String, hypervisor_id: String, vm_id: String, force: bool) -> Result<VirtualMachine, ApiError> {
    with_domain(uri, vm_id, move |domain| {
        if force {
            domain.destroy()?;
        } else {
            domain.shutdown()?;
        }
        to_vm(domain, &hypervisor_id)
    })
    .await
}

/// Opens a connection, finds the domain from its UUID and runs `action` on it.
async fn with_domain<T, F>(uri: String, vm_id: String, action: F) -> Result<T, ApiError>
where
    F: FnOnce(&Domain) -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    // Also protects the C call below: a UUID with a NUL byte would make it panic.
    Uuid::parse_str(&vm_id).map_err(|_| ApiError::BadRequest("Invalid VM identifier".into()))?;
    run_blocking(move || {
        let conn = Connect::open(Some(&uri))?;
        let domain = Domain::lookup_by_uuid_string(&conn, &vm_id)?;
        action(&domain)
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
