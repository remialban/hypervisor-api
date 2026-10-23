use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{StatusCode, header},
};
use serde::Deserialize;
use utoipa::IntoParams;

use crate::{
    access::{self, Usage},
    audit,
    auth::AuthUser,
    errors::ApiError,
    jobs::Job,
    libvirt,
    models::{CloneVm, CreateVm, Message, VirtualMachine},
    state::SharedState,
    vm_create, vm_xml,
};

fn audit_target(hypervisor_id: &str, vm_id: &str) -> String {
    format!("{hypervisor_id}/{vm_id}")
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/vms", params(("hypervisor_id" = String, Path)), responses((status = 200, body = [VirtualMachine]), (status = 404, body = Message), (status = 502, body = Message)), tag = "VMs")]
pub async fn list(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(hypervisor_id): Path<String>,
) -> Result<Json<Vec<VirtualMachine>>, ApiError> {
    if !access::hypervisor_visible(&state, &user, &hypervisor_id).await? {
        return Err(ApiError::NotFound("Hypervisor not found".into()));
    }
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    let vms = libvirt::list_vms(uri, hypervisor_id.clone()).await?;
    Ok(Json(
        access::visible_vms(&state, &user, &hypervisor_id, vms).await?,
    ))
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), responses((status = 200, body = VirtualMachine), (status = 400, body = Message), (status = 404, body = Message), (status = 502, body = Message)), tag = "VMs")]
pub async fn get(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, vm_id)): Path<(String, String)>,
) -> Result<Json<VirtualMachine>, ApiError> {
    let vm_access = access::vm_access(&state, &user, &hypervisor_id, &vm_id).await?;
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    let vm = libvirt::get_vm(uri, hypervisor_id, vm_id).await?;
    Ok(Json(
        access::describe_vm(&state, &user, vm_access, vm).await?,
    ))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/vms", params(("hypervisor_id" = String, Path)), request_body = CreateVm, responses((status = 201, body = VirtualMachine), (status = 400, body = Message), (status = 404, body = Message, description = "Hypervisor, pool, volume or network not found"), (status = 409, body = Message), (status = 502, body = Message)), tag = "VMs")]
pub async fn create(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(hypervisor_id): Path<String>,
    Json(body): Json<CreateVm>,
) -> Result<(StatusCode, Json<VirtualMachine>), ApiError> {
    // Creating a VM needs a grant on the hypervisor (admins have it everywhere).
    access::require_grant(&state, &user, &hypervisor_id).await?;
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;

    // Only an admin can create a VM for someone else.
    let owner_id = match &body.owner_id {
        Some(owner_id) => {
            user.require_admin()?;
            let exists: Option<i64> =
                sqlx::query_scalar("SELECT 1 FROM users WHERE id = ? AND disabled = 0")
                    .bind(owner_id)
                    .fetch_optional(&state.db)
                    .await?;
            exists.ok_or_else(|| ApiError::NotFound("Owner not found".into()))?;
            owner_id.clone()
        }
        None => user.id.clone(),
    };

    vm_create::validate(&body)?;
    let new_disks_gb: u64 = body.disks.iter().filter_map(|d| d.size_gb).sum();
    let usage = Usage::one_vm(body.vcpus, body.memory_mb, new_disks_gb);
    let vnc_listen = vm_create::vnc_listen()?;

    // Under the lock, so that two requests cannot both pass the check of the last free slot.
    let _quota = state.quota_lock.lock().await;
    access::check_quota(&state, &owner_id, usage).await?;
    let vm = libvirt::create_vm(uri, hypervisor_id.clone(), body, vnc_listen).await?;
    access::record_owner(&state, &hypervisor_id, &vm.id, &owner_id, usage).await?;
    audit::record(
        &state,
        &user,
        "vm.create",
        &audit_target(&hypervisor_id, &vm.id),
        &vm.name,
    )
    .await;
    let vm_access = if user.is_admin() {
        access::VmAccess::Admin
    } else {
        access::VmAccess::Owner
    };
    Ok((
        StatusCode::CREATED,
        Json(access::describe_vm(&state, &user, vm_access, vm).await?),
    ))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/start", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), responses((status = 200, body = VirtualMachine), (status = 400, body = Message), (status = 404, body = Message), (status = 409, body = Message), (status = 502, body = Message)), tag = "VMs")]
pub async fn start(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, vm_id)): Path<(String, String)>,
) -> Result<Json<VirtualMachine>, ApiError> {
    let vm_access = access::vm_access(&state, &user, &hypervisor_id, &vm_id).await?;
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    let target = audit_target(&hypervisor_id, &vm_id);
    let vm = libvirt::start_vm(uri, hypervisor_id, vm_id).await?;
    audit::record(&state, &user, "vm.start", &target, &vm.name).await;
    Ok(Json(
        access::describe_vm(&state, &user, vm_access, vm).await?,
    ))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StopQuery {
    /// Cut the power instead of asking the guest to shut down (default: false).
    force: Option<bool>,
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/stop", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path), StopQuery), responses((status = 200, body = VirtualMachine), (status = 400, body = Message), (status = 404, body = Message), (status = 409, body = Message), (status = 502, body = Message)), tag = "VMs")]
pub async fn stop(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, vm_id)): Path<(String, String)>,
    Query(query): Query<StopQuery>,
) -> Result<Json<VirtualMachine>, ApiError> {
    let vm_access = access::vm_access(&state, &user, &hypervisor_id, &vm_id).await?;
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    let force = query.force.unwrap_or(false);
    let target = audit_target(&hypervisor_id, &vm_id);
    let vm = libvirt::stop_vm(uri, hypervisor_id, vm_id, force).await?;
    let action = if force { "vm.stop_force" } else { "vm.stop" };
    audit::record(&state, &user, action, &target, &vm.name).await;
    Ok(Json(
        access::describe_vm(&state, &user, vm_access, vm).await?,
    ))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/clone", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)), request_body = CloneVm, responses((status = 202, body = Job, description = "Clone started: follow it with GET /api/jobs/{job_id}"), (status = 400, body = Message), (status = 404, body = Message)), tag = "VMs")]
pub async fn clone(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, vm_id)): Path<(String, String)>,
    Json(body): Json<CloneVm>,
) -> Result<(StatusCode, [(header::HeaderName, String); 1], Json<Job>), ApiError> {
    access::vm_access(&state, &user, &hypervisor_id, &vm_id)
        .await?
        .require_owner()?;
    // Cheap checks first, so obvious mistakes fail immediately instead of inside the job.
    vm_xml::validate_name(&body.name)?;
    let vm_id = access::parse_vm_id(&vm_id)?;
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;

    // The clone belongs to whoever asks for it, and counts in their quota.
    let source: Option<(i64, i64, i64)> = sqlx::query_as(
        "SELECT vcpus, memory_mb, disk_gb FROM vm_owners WHERE hypervisor_id = ? AND vm_id = ?",
    )
    .bind(&hypervisor_id)
    .bind(&vm_id)
    .fetch_optional(&state.db)
    .await?;
    let (vcpus, memory_mb, disk_gb) = source.unwrap_or((0, 0, 0));
    let usage = Usage {
        vms: 1,
        vcpus,
        memory_mb,
        disk_gb,
    };

    let job = {
        let _quota = state.quota_lock.lock().await;
        access::check_quota(&state, &user.id, usage).await?;
        state.jobs.start("clone", &user.id, usage)
    };
    audit::record(
        &state,
        &user,
        "vm.clone",
        &audit_target(&hypervisor_id, &vm_id),
        &body.name,
    )
    .await;

    let job_id = job.id.clone();
    let task_state = Arc::clone(&state);
    let owner_id = user.id.clone();
    tokio::spawn(async move {
        let outcome = libvirt::clone_vm(uri, hypervisor_id.clone(), vm_id, body.name).await;
        // The owner is saved before the job is declared finished: no window where the
        // client sees a clone it cannot access.
        let outcome = match outcome {
            Ok(vm) => access::record_owner(&task_state, &hypervisor_id, &vm.id, &owner_id, usage)
                .await
                .map(|_| vm),
            Err(error) => Err(error),
        };
        task_state.jobs.finish(&job_id, outcome);
    });

    let location = format!("/api/jobs/{}", job.id);
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, location)],
        Json(job),
    ))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DeleteQuery {
    /// Also delete the disk volumes of the VM (default: false, only the definition is removed).
    delete_disks: Option<bool>,
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}", params(("hypervisor_id" = String, Path), ("vm_id" = String, Path), DeleteQuery), responses((status = 204), (status = 400, body = Message), (status = 404, body = Message), (status = 409, body = Message), (status = 502, body = Message)), tag = "VMs")]
pub async fn delete(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, vm_id)): Path<(String, String)>,
    Query(query): Query<DeleteQuery>,
) -> Result<StatusCode, ApiError> {
    access::vm_access(&state, &user, &hypervisor_id, &vm_id)
        .await?
        .require_owner()?;
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    libvirt::delete_vm(uri, vm_id.clone(), query.delete_disks.unwrap_or(false)).await?;
    access::forget_vm(&state, &hypervisor_id, &vm_id).await;
    audit::record(
        &state,
        &user,
        "vm.delete",
        &audit_target(&hypervisor_id, &vm_id),
        "",
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
