//! Labs: several VMs made from templates, wired to several networks, deployed as instances.
//!
//! A lab is a definition, tied to one hypervisor:
//! * networks: `external` ones are existing libvirt networks of the hypervisor (NAT or bridge
//!   to the outside, shared by every instance); `internal` ones are isolated networks,
//!   created for each instance;
//! * VMs: each from a template (its latest built version, or a given one), with its NICs in
//!   order: the first NIC is the first one the guest sees. A `router` VM (OPNsense...) routes
//!   between networks: it is only wired here, its own configuration comes from its template.
//!   Routers are created and started before the other VMs.
//!
//! Deploying a lab makes an instance: its internal networks are created, then its VMs, owned
//! by whoever deployed it (they count in their quota, and the usual VM routes work on them:
//! console, start, stop). An instance is deleted as a whole: VMs, their disks and its
//! internal networks.
//!
//! Rights, as for templates (`.../permissions`, to users or groups, nested groups included):
//! * admins create and delete labs, choose their visibility and give rights;
//! * `manage`: edit the lab (networks, VMs) and see and act on every instance of it;
//! * `use`: see the lab, deploy instances (at most `max_instances` per user, the most
//!   generous right winning) and act on one's own instances;
//! * a `hypervisor` lab can also be used, without limit, by every user granted its hypervisor;
//!   a `restricted` one (the default) only through rights.
//!
//! A lab manager can only put in a lab the templates they may use themselves. Deploying a lab
//! does not need rights on its templates, nor counts in their `max_vms`.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{StatusCode, header},
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;
use virt::{connect::Connect, domain::Domain, error::ErrorNumber, network::Network, sys};
use xmltree::{Element, XMLNode};

use crate::{
    access::{self, Usage},
    audit,
    auth::{AuthUser, now},
    errors::ApiError,
    groups, libvirt,
    models::Message,
    state::SharedState,
    templates::{
        self, AccessLevel, NewVm, PrincipalKind, TemplateRole, Visibility, blocking, check_new_vm,
        define_from_template,
    },
    vm_create, vm_options, vm_xml,
};

const MAX_NAME_LEN: usize = 64;
const MAX_DESCRIPTION_LEN: usize = 2000;
/// Name of a lab network: it goes into the name of the libvirt network.
const MAX_NETWORK_NAME_LEN: usize = 32;
/// Name of a lab VM: the libvirt name adds `-` and 8 characters.
const MAX_VM_NAME_LEN: usize = 55;
const MAX_NICS: usize = 8;
const MAX_VMS_PER_LAB: i64 = 32;
const MAX_NETWORKS_PER_LAB: i64 = 16;
const INTERRUPTED: &str = "The deployment was interrupted by a restart of the API";

/// Deploying instances created before this time belong to a previous run of the API.
static STARTED_AT: LazyLock<i64> = LazyLock::new(now);

// ---------- models ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum NetworkKind {
    /// An existing libvirt network of the hypervisor (internet access), shared by instances.
    External,
    /// An isolated network created for each instance.
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum VmRole {
    Vm,
    /// Routes between the networks of the lab (OPNsense...): created and started first.
    Router,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum InstanceStatus {
    /// Networks and VMs are being created.
    Deploying,
    Ready,
    /// The deployment failed (see `error`): what it had created was removed.
    Failed,
}

#[derive(FromRow)]
struct LabRow {
    id: String,
    hypervisor_id: String,
    name: String,
    description: String,
    visibility: Visibility,
    created_at: i64,
    updated_at: i64,
}

const LAB_COLUMNS: &str =
    "id, hypervisor_id, name, description, visibility, created_at, updated_at";

/// What the caller may do with a lab.
#[derive(Serialize, ToSchema)]
pub struct LabAccess {
    pub level: AccessLevel,
    /// How many instances of this lab the caller may own; null: no limit.
    pub max_instances: Option<i64>,
    /// Instances of this lab the caller owns.
    pub instance_count: i64,
}

#[derive(Serialize, ToSchema)]
pub struct Lab {
    pub id: String,
    pub hypervisor_id: String,
    pub name: String,
    pub description: String,
    pub visibility: Visibility,
    pub access: LabAccess,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A lab with its networks and its VMs.
#[derive(Serialize, ToSchema)]
pub struct LabDetail {
    #[serde(flatten)]
    pub lab: Lab,
    pub networks: Vec<LabNetwork>,
    pub vms: Vec<LabVm>,
}

#[derive(Serialize, ToSchema, FromRow)]
pub struct LabNetwork {
    pub id: String,
    pub name: String,
    pub kind: NetworkKind,
    /// For an external network: the libvirt network it is.
    pub libvirt_network: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct LabVm {
    pub id: String,
    pub name: String,
    pub role: VmRole,
    pub template_id: String,
    pub template_name: String,
    /// null: the latest built version of the template.
    pub version: Option<i64>,
    pub memory_mb: i64,
    pub vcpus: i64,
    pub firmware: String,
    /// Ids of the lab networks of its NICs, in order (the first one is the first NIC).
    pub networks: Vec<String>,
}

#[derive(FromRow)]
struct LabVmRow {
    id: String,
    name: String,
    role: VmRole,
    template_id: String,
    template_name: String,
    version: Option<i64>,
    memory_mb: i64,
    vcpus: i64,
    firmware: String,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateLab {
    pub name: String,
    pub description: Option<String>,
    /// Default: `restricted`.
    pub visibility: Option<Visibility>,
}

/// Fields left out keep their value. `visibility` is for admins only.
#[derive(Deserialize, ToSchema)]
pub struct UpdateLab {
    pub name: Option<String>,
    pub description: Option<String>,
    pub visibility: Option<Visibility>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateLabNetwork {
    /// Letters, digits, `-` and `_`.
    pub name: String,
    pub kind: NetworkKind,
    /// For an external network: an existing libvirt network of the hypervisor (`default`...).
    pub libvirt_network: Option<String>,
}

/// Body of `POST .../vms` and `PUT .../vms/{lab_vm_id}` (the whole VM).
#[derive(Deserialize, ToSchema)]
pub struct LabVmBody {
    pub name: String,
    /// Default: `vm`.
    pub role: Option<VmRole>,
    /// A template of the same hypervisor, that the caller may use.
    pub template_id: String,
    /// Version of the template; null or absent: its latest built version at deployment.
    pub version: Option<i64>,
    pub memory_mb: u64,
    pub vcpus: u64,
    /// `bios` (default) or `uefi`.
    pub firmware: Option<String>,
    /// Ids of lab networks, one NIC each, in order. For an OPNsense: WAN first.
    #[serde(default)]
    pub networks: Vec<String>,
}

/// Body of `PUT .../permissions/users/{user_id}` and `.../permissions/groups/{group_id}`.
#[derive(Deserialize, ToSchema)]
pub struct SetLabPermission {
    /// Default: `use`.
    pub role: Option<TemplateRole>,
    /// Instances of this lab each user covered may own. Absent or null: no limit.
    pub max_instances: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct LabPermission {
    pub kind: PrincipalKind,
    /// Id of the user or of the group.
    pub id: String,
    /// Username, or group name.
    pub name: String,
    pub role: TemplateRole,
    pub max_instances: Option<i64>,
    /// For a user: the instances of this lab they own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_count: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateInstance {
    /// Default: the name of the lab.
    pub name: Option<String>,
    /// Start the VMs once created, routers first (default: false).
    pub start: Option<bool>,
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StopQuery {
    /// Power off at once instead of an ACPI shutdown (default: false).
    force: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct LabInstance {
    pub id: String,
    pub lab_id: String,
    pub lab_name: String,
    pub hypervisor_id: String,
    pub owner_id: String,
    pub owner: String,
    pub name: String,
    pub status: InstanceStatus,
    /// Why the deployment failed.
    pub error: Option<String>,
    pub created_at: i64,
    pub networks: Vec<InstanceNetwork>,
    pub vms: Vec<InstanceVm>,
}

#[derive(Serialize, ToSchema, FromRow)]
pub struct InstanceNetwork {
    pub network_id: String,
    pub name: String,
    pub kind: NetworkKind,
    /// The libvirt network on the hypervisor.
    pub libvirt_network: String,
}

#[derive(Serialize, ToSchema)]
pub struct InstanceVm {
    pub lab_vm_id: String,
    /// libvirt UUID: the VM routes (`/api/hypervisors/{id}/vms/{vm_id}/...`) work on it.
    pub vm_id: String,
    pub name: String,
    pub role: VmRole,
    /// `running`, `stopped`... as for VMs; `missing` if it is gone from the hypervisor.
    pub status: String,
    /// libvirt networks of its NICs, in order.
    pub networks: Vec<String>,
}

/// An instance owned by the caller, in any lab (`GET /api/me/lab-instances`).
#[derive(Serialize, ToSchema, FromRow)]
pub struct MyInstance {
    pub id: String,
    pub name: String,
    pub status: InstanceStatus,
    pub lab_id: String,
    pub lab_name: String,
    pub hypervisor_id: String,
    pub created_at: i64,
}

type Accepted<T> = (StatusCode, [(header::HeaderName, String); 1], Json<T>);

fn lab_path(hypervisor_id: &str, lab_id: &str) -> String {
    format!("/api/hypervisors/{hypervisor_id}/labs/{lab_id}")
}

// ---------- labs ----------

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/labs", params(("hypervisor_id" = String, Path)), responses((status = 200, body = [Lab], description = "The labs the caller has a right on (admins: all)"), (status = 404, body = Message)), tag = "Labs")]
pub async fn list(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(hypervisor_id): Path<String>,
) -> Result<Json<Vec<Lab>>, ApiError> {
    libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    let caller = Caller::of(&state, &user, &hypervisor_id).await?;
    let rows = sqlx::query_as::<_, LabRow>(&format!(
        "SELECT {LAB_COLUMNS} FROM labs WHERE hypervisor_id = $1 ORDER BY name"
    ))
    .bind(&hypervisor_id)
    .fetch_all(&state.db)
    .await?;
    let mut labs = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(rights) = caller.rights(&state, &row).await? {
            labs.push(view(&state, &user, row, rights).await?);
        }
    }
    Ok(Json(labs))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/labs", params(("hypervisor_id" = String, Path)), request_body = CreateLab, responses((status = 201, body = LabDetail), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message), (status = 409, body = Message, description = "A lab with this name already exists on this hypervisor")), tag = "Labs")]
pub async fn create(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(hypervisor_id): Path<String>,
    Json(body): Json<CreateLab>,
) -> Result<(StatusCode, Json<LabDetail>), ApiError> {
    user.require_admin()?;
    libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    let name = validate_name(&body.name)?;
    let description = validate_description(body.description.as_deref().unwrap_or(""))?;
    let id = Uuid::new_v4().to_string();
    let at = now();
    sqlx::query(
        "INSERT INTO labs (id, hypervisor_id, name, description, visibility, created_by, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $7)",
    )
    .bind(&id)
    .bind(&hypervisor_id)
    .bind(&name)
    .bind(&description)
    .bind(body.visibility.unwrap_or(Visibility::Restricted))
    .bind(&user.id)
    .bind(at)
    .execute(&state.db)
    .await
    .map_err(|e| conflict(e, "A lab with this name already exists on this hypervisor"))?;
    audit::record(
        &state,
        &user,
        "lab.create",
        &format!("{hypervisor_id}/{id}"),
        &name,
    )
    .await;
    let (row, rights) = find_lab(&state, &user, &hypervisor_id, &id).await?;
    Ok((
        StatusCode::CREATED,
        Json(detail(&state, &user, row, rights).await?),
    ))
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path)), responses((status = 200, body = LabDetail), (status = 404, body = Message)), tag = "Labs")]
pub async fn get(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id)): Path<(String, String)>,
) -> Result<Json<LabDetail>, ApiError> {
    let (row, rights) = find_lab(&state, &user, &hypervisor_id, &lab_id).await?;
    Ok(Json(detail(&state, &user, row, rights).await?))
}

#[utoipa::path(patch, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path)), request_body = UpdateLab, responses((status = 200, body = LabDetail), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message), (status = 409, body = Message)), tag = "Labs")]
pub async fn update(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id)): Path<(String, String)>,
    Json(body): Json<UpdateLab>,
) -> Result<Json<LabDetail>, ApiError> {
    let (row, rights) = find_lab(&state, &user, &hypervisor_id, &lab_id).await?;
    rights.require_manage()?;
    // Who may use the lab is decided by admins.
    if body.visibility.is_some() {
        user.require_admin()?;
    }
    let name = match &body.name {
        Some(name) => validate_name(name)?,
        None => row.name,
    };
    let description = match &body.description {
        Some(description) => validate_description(description)?,
        None => row.description,
    };
    sqlx::query(
        "UPDATE labs SET name = $1, description = $2, visibility = $3, updated_at = $4 WHERE id = $5",
    )
    .bind(&name)
    .bind(&description)
    .bind(body.visibility.unwrap_or(row.visibility))
    .bind(now())
    .bind(&row.id)
    .execute(&state.db)
    .await
    .map_err(|e| conflict(e, "A lab with this name already exists on this hypervisor"))?;
    audit::record(
        &state,
        &user,
        "lab.update",
        &format!("{hypervisor_id}/{lab_id}"),
        &name,
    )
    .await;
    let (row, rights) = find_lab(&state, &user, &hypervisor_id, &lab_id).await?;
    Ok(Json(detail(&state, &user, row, rights).await?))
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path)), responses((status = 204), (status = 403, body = Message), (status = 404, body = Message), (status = 409, body = Message, description = "The lab still has instances")), tag = "Labs")]
pub async fn delete(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    let (row, _) = find_lab(&state, &user, &hypervisor_id, &lab_id).await?;
    let instances: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lab_instances WHERE lab_id = $1")
        .bind(&row.id)
        .fetch_one(&state.db)
        .await?;
    if instances > 0 {
        return Err(ApiError::Conflict(format!(
            "The lab still has {instances} instance(s): delete them first"
        )));
    }
    sqlx::query("DELETE FROM labs WHERE id = $1")
        .bind(&row.id)
        .execute(&state.db)
        .await?;
    audit::record(
        &state,
        &user,
        "lab.delete",
        &format!("{hypervisor_id}/{lab_id}"),
        &row.name,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- networks ----------

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/networks", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path)), request_body = CreateLabNetwork, responses((status = 201, body = LabNetwork), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message, description = "Lab, or libvirt network of an external network, not found"), (status = 409, body = Message)), tag = "Labs")]
pub async fn add_network(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id)): Path<(String, String)>,
    Json(body): Json<CreateLabNetwork>,
) -> Result<(StatusCode, Json<LabNetwork>), ApiError> {
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    let (row, rights) = find_lab(&state, &user, &hypervisor_id, &lab_id).await?;
    rights.require_manage()?;
    let name = validate_network_name(&body.name)?;
    let libvirt_network = match (body.kind, body.libvirt_network) {
        (NetworkKind::External, Some(network)) => {
            vm_xml::validate_name(&network)
                .map_err(|_| ApiError::BadRequest("Invalid libvirt_network".into()))?;
            let checked = network.clone();
            blocking(move || {
                let conn = Connect::open(Some(&uri))?;
                Network::lookup_by_name(&conn, &checked).map_err(|e| match e.code() {
                    ErrorNumber::NoNetwork => ApiError::NotFound(format!(
                        "The hypervisor has no libvirt network named {checked}"
                    )),
                    _ => e.into(),
                })?;
                Ok(())
            })
            .await?;
            Some(network)
        }
        (NetworkKind::External, None) => {
            return Err(ApiError::BadRequest(
                "An external network needs libvirt_network: an existing network of the hypervisor"
                    .into(),
            ));
        }
        (NetworkKind::Internal, None) => None,
        (NetworkKind::Internal, Some(_)) => {
            return Err(ApiError::BadRequest(
                "An internal network is created for each instance: no libvirt_network".into(),
            ));
        }
    };
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lab_networks WHERE lab_id = $1")
        .bind(&row.id)
        .fetch_one(&state.db)
        .await?;
    if count >= MAX_NETWORKS_PER_LAB {
        return Err(ApiError::BadRequest(format!(
            "A lab has at most {MAX_NETWORKS_PER_LAB} networks"
        )));
    }
    let network = LabNetwork {
        id: Uuid::new_v4().to_string(),
        name,
        kind: body.kind,
        libvirt_network,
    };
    sqlx::query(
        "INSERT INTO lab_networks (id, lab_id, name, kind, libvirt_network, created_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(&network.id)
    .bind(&row.id)
    .bind(&network.name)
    .bind(network.kind)
    .bind(&network.libvirt_network)
    .bind(now())
    .execute(&state.db)
    .await
    .map_err(|e| conflict(e, "The lab already has a network with this name"))?;
    touch(&state, &row.id).await?;
    audit::record(
        &state,
        &user,
        "lab.network_add",
        &format!("{hypervisor_id}/{lab_id}"),
        &network.name,
    )
    .await;
    Ok((StatusCode::CREATED, Json(network)))
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/networks/{network_id}", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path), ("network_id" = String, Path)), responses((status = 204), (status = 403, body = Message), (status = 404, body = Message), (status = 409, body = Message, description = "A VM of the lab, or an instance, uses it")), tag = "Labs")]
pub async fn remove_network(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id, network_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let (row, rights) = find_lab(&state, &user, &hypervisor_id, &lab_id).await?;
    rights.require_manage()?;
    let name: Option<String> =
        sqlx::query_scalar("SELECT name FROM lab_networks WHERE id = $1 AND lab_id = $2")
            .bind(&network_id)
            .bind(&row.id)
            .fetch_optional(&state.db)
            .await?;
    let name = name.ok_or_else(|| ApiError::NotFound("Network not found".into()))?;
    let used_by: Option<String> = sqlx::query_scalar(
        "SELECT v.name FROM lab_vm_nics n JOIN lab_vms v ON v.id = n.lab_vm_id WHERE n.network_id = $1 LIMIT 1",
    )
    .bind(&network_id)
    .fetch_optional(&state.db)
    .await?;
    if let Some(vm) = used_by {
        return Err(ApiError::Conflict(format!(
            "The VM {vm} of the lab is plugged into this network: unplug it first"
        )));
    }
    ensure_unused_by_instances(&state, "lab_instance_networks", "network_id", &network_id).await?;
    sqlx::query("DELETE FROM lab_networks WHERE id = $1")
        .bind(&network_id)
        .execute(&state.db)
        .await?;
    touch(&state, &row.id).await?;
    audit::record(
        &state,
        &user,
        "lab.network_remove",
        &format!("{hypervisor_id}/{lab_id}"),
        &name,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- VMs ----------

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/vms", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path)), request_body = LabVmBody, responses((status = 201, body = LabVm), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message, description = "Lab, template, version or lab network not found"), (status = 409, body = Message)), tag = "Labs")]
pub async fn add_vm(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id)): Path<(String, String)>,
    Json(body): Json<LabVmBody>,
) -> Result<(StatusCode, Json<LabVm>), ApiError> {
    let (row, rights) = find_lab(&state, &user, &hypervisor_id, &lab_id).await?;
    rights.require_manage()?;
    let vm = validate_vm(&state, &user, &row, &body).await?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lab_vms WHERE lab_id = $1")
        .bind(&row.id)
        .fetch_one(&state.db)
        .await?;
    if count >= MAX_VMS_PER_LAB {
        return Err(ApiError::BadRequest(format!(
            "A lab has at most {MAX_VMS_PER_LAB} VMs"
        )));
    }
    let id = Uuid::new_v4().to_string();
    let at = now();
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "INSERT INTO lab_vms (id, lab_id, name, role, template_id, version, memory_mb, vcpus, firmware, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10)",
    )
    .bind(&id)
    .bind(&row.id)
    .bind(&vm.name)
    .bind(vm.role)
    .bind(&vm.template_id)
    .bind(vm.version)
    .bind(vm.memory_mb)
    .bind(vm.vcpus)
    .bind(&vm.firmware)
    .bind(at)
    .execute(&mut *tx)
    .await
    .map_err(|e| conflict(e, "The lab already has a VM with this name"))?;
    insert_nics(&mut tx, &id, &vm.networks).await?;
    tx.commit().await?;
    touch(&state, &row.id).await?;
    audit::record(
        &state,
        &user,
        "lab.vm_add",
        &format!("{hypervisor_id}/{lab_id}"),
        &vm.name,
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(load_vm(&state, &row.id, &id).await?),
    ))
}

#[utoipa::path(put, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/vms/{lab_vm_id}", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path), ("lab_vm_id" = String, Path)), request_body = LabVmBody, responses((status = 200, body = LabVm, description = "Replaced; instances already deployed keep what they have"), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message), (status = 409, body = Message)), tag = "Labs")]
pub async fn update_vm(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id, lab_vm_id)): Path<(String, String, String)>,
    Json(body): Json<LabVmBody>,
) -> Result<Json<LabVm>, ApiError> {
    let (row, rights) = find_lab(&state, &user, &hypervisor_id, &lab_id).await?;
    rights.require_manage()?;
    load_vm(&state, &row.id, &lab_vm_id).await?;
    let vm = validate_vm(&state, &user, &row, &body).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE lab_vms SET name = $1, role = $2, template_id = $3, version = $4, memory_mb = $5,
         vcpus = $6, firmware = $7, updated_at = $8 WHERE id = $9",
    )
    .bind(&vm.name)
    .bind(vm.role)
    .bind(&vm.template_id)
    .bind(vm.version)
    .bind(vm.memory_mb)
    .bind(vm.vcpus)
    .bind(&vm.firmware)
    .bind(now())
    .bind(&lab_vm_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| conflict(e, "The lab already has a VM with this name"))?;
    sqlx::query("DELETE FROM lab_vm_nics WHERE lab_vm_id = $1")
        .bind(&lab_vm_id)
        .execute(&mut *tx)
        .await?;
    insert_nics(&mut tx, &lab_vm_id, &vm.networks).await?;
    tx.commit().await?;
    touch(&state, &row.id).await?;
    audit::record(
        &state,
        &user,
        "lab.vm_update",
        &format!("{hypervisor_id}/{lab_id}"),
        &vm.name,
    )
    .await;
    Ok(Json(load_vm(&state, &row.id, &lab_vm_id).await?))
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/vms/{lab_vm_id}", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path), ("lab_vm_id" = String, Path)), responses((status = 204), (status = 403, body = Message), (status = 404, body = Message), (status = 409, body = Message, description = "An instance still has this VM")), tag = "Labs")]
pub async fn remove_vm(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id, lab_vm_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let (row, rights) = find_lab(&state, &user, &hypervisor_id, &lab_id).await?;
    rights.require_manage()?;
    let vm = load_vm(&state, &row.id, &lab_vm_id).await?;
    ensure_unused_by_instances(&state, "lab_instance_vms", "lab_vm_id", &lab_vm_id).await?;
    sqlx::query("DELETE FROM lab_vms WHERE id = $1")
        .bind(&lab_vm_id)
        .execute(&state.db)
        .await?;
    touch(&state, &row.id).await?;
    audit::record(
        &state,
        &user,
        "lab.vm_remove",
        &format!("{hypervisor_id}/{lab_id}"),
        &vm.name,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// A validated `LabVmBody`, ready to store.
struct ValidVm {
    name: String,
    role: VmRole,
    template_id: String,
    version: Option<i64>,
    memory_mb: i64,
    vcpus: i64,
    firmware: String,
    networks: Vec<String>,
}

async fn validate_vm(
    state: &SharedState,
    user: &AuthUser,
    lab: &LabRow,
    body: &LabVmBody,
) -> Result<ValidVm, ApiError> {
    vm_xml::validate_name(&body.name)?;
    if body.name.len() > MAX_VM_NAME_LEN {
        return Err(ApiError::BadRequest(format!(
            "The name of a lab VM has at most {MAX_VM_NAME_LEN} characters"
        )));
    }
    in_range(
        "memory_mb",
        body.memory_mb,
        vm_options::MIN_MEMORY_MB,
        vm_options::MAX_MEMORY_MB,
    )?;
    in_range(
        "vcpus",
        body.vcpus,
        vm_options::MIN_VCPUS,
        vm_options::MAX_VCPUS,
    )?;
    let firmware = body
        .firmware
        .clone()
        .unwrap_or_else(|| vm_options::DEFAULT_FIRMWARE.into());
    if !vm_options::FIRMWARES.contains(&firmware.as_str()) {
        return Err(ApiError::BadRequest(format!(
            "firmware must be one of {:?}",
            vm_options::FIRMWARES
        )));
    }
    if body.networks.len() > MAX_NICS {
        return Err(ApiError::BadRequest(format!(
            "A VM has at most {MAX_NICS} networks"
        )));
    }
    // Only templates the caller may use, of the lab's hypervisor.
    if !templates::can_use(state, user, &lab.hypervisor_id, &body.template_id).await? {
        return Err(ApiError::NotFound("Template not found".into()));
    }
    if let Some(version) = body.version {
        let exists: Option<i64> = sqlx::query_scalar(
            "SELECT 1::BIGINT FROM template_versions WHERE template_id = $1 AND version = $2",
        )
        .bind(&body.template_id)
        .bind(version)
        .fetch_optional(&state.db)
        .await?;
        exists.ok_or_else(|| ApiError::NotFound("Template version not found".into()))?;
    }
    let lab_networks: Vec<String> =
        sqlx::query_scalar("SELECT id FROM lab_networks WHERE lab_id = $1")
            .bind(&lab.id)
            .fetch_all(&state.db)
            .await?;
    if let Some(unknown) = body.networks.iter().find(|n| !lab_networks.contains(n)) {
        return Err(ApiError::NotFound(format!(
            "The lab has no network {unknown}"
        )));
    }
    Ok(ValidVm {
        name: body.name.clone(),
        role: body.role.unwrap_or(VmRole::Vm),
        template_id: body.template_id.clone(),
        version: body.version,
        memory_mb: body.memory_mb as i64,
        vcpus: body.vcpus as i64,
        firmware,
        networks: body.networks.clone(),
    })
}

async fn insert_nics(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    lab_vm_id: &str,
    networks: &[String],
) -> Result<(), ApiError> {
    for (position, network_id) in networks.iter().enumerate() {
        sqlx::query(
            "INSERT INTO lab_vm_nics (lab_vm_id, position, network_id) VALUES ($1, $2, $3)",
        )
        .bind(lab_vm_id)
        .bind(position as i64)
        .bind(network_id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

// ---------- permissions ----------

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/permissions", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path)), responses((status = 200, body = [LabPermission], description = "Users first, then groups"), (status = 403, body = Message), (status = 404, body = Message)), tag = "Labs")]
pub async fn list_permissions(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id)): Path<(String, String)>,
) -> Result<Json<Vec<LabPermission>>, ApiError> {
    user.require_admin()?;
    let (row, _) = find_lab(&state, &user, &hypervisor_id, &lab_id).await?;
    let entries = sqlx::query_as::<_, PermissionRow>(
        "SELECT 'user' AS kind, p.user_id AS id, u.username AS name, p.role, p.max_instances, p.created_at, p.updated_at
         FROM lab_permissions p JOIN users u ON u.id = p.user_id WHERE p.lab_id = $1
         UNION ALL
         SELECT 'group', p.group_id, g.name, p.role, p.max_instances, p.created_at, p.updated_at
         FROM lab_permissions p JOIN user_groups g ON g.id = p.group_id WHERE p.lab_id = $1
         ORDER BY 1 DESC, 3",
    )
    .bind(&row.id)
    .fetch_all(&state.db)
    .await?;
    let mut permissions = Vec::with_capacity(entries.len());
    for entry in entries {
        let (kind, instance_count) = if entry.kind == "user" {
            (
                PrincipalKind::User,
                Some(instances_of(&state, &row.id, &entry.id).await?),
            )
        } else {
            (PrincipalKind::Group, None)
        };
        permissions.push(LabPermission {
            kind,
            id: entry.id,
            name: entry.name,
            role: entry.role,
            max_instances: entry.max_instances,
            instance_count,
            created_at: entry.created_at,
            updated_at: entry.updated_at,
        });
    }
    Ok(Json(permissions))
}

#[derive(FromRow)]
struct PermissionRow {
    /// `user` or `group`.
    kind: String,
    id: String,
    name: String,
    role: TemplateRole,
    max_instances: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

#[utoipa::path(put, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/permissions/users/{user_id}", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path), ("user_id" = String, Path)), request_body = SetLabPermission, responses((status = 204, description = "Right given or changed"), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message)), tag = "Labs")]
pub async fn set_user_permission(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id, user_id)): Path<(String, String, String)>,
    Json(body): Json<SetLabPermission>,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    let (row, _) = find_lab(&state, &user, &hypervisor_id, &lab_id).await?;
    let name: Option<String> = sqlx::query_scalar("SELECT username FROM users WHERE id = $1")
        .bind(&user_id)
        .fetch_optional(&state.db)
        .await?;
    let name = name.ok_or_else(|| ApiError::NotFound("User not found".into()))?;
    set_permission(&state, &row.id, "user_id", &user_id, &body).await?;
    audit::record(
        &state,
        &user,
        "lab.permission",
        &format!("{hypervisor_id}/{lab_id}"),
        &permission_detail("user", &name, &body),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(put, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/permissions/groups/{group_id}", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path), ("group_id" = String, Path)), request_body = SetLabPermission, responses((status = 204, description = "Right given or changed, for every user of the group and of its nested groups"), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message)), tag = "Labs")]
pub async fn set_group_permission(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id, group_id)): Path<(String, String, String)>,
    Json(body): Json<SetLabPermission>,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    let (row, _) = find_lab(&state, &user, &hypervisor_id, &lab_id).await?;
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM user_groups WHERE id = $1")
        .bind(&group_id)
        .fetch_optional(&state.db)
        .await?;
    let name = name.ok_or_else(|| ApiError::NotFound("Group not found".into()))?;
    set_permission(&state, &row.id, "group_id", &group_id, &body).await?;
    audit::record(
        &state,
        &user,
        "lab.permission",
        &format!("{hypervisor_id}/{lab_id}"),
        &permission_detail("group", &name, &body),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/permissions/users/{user_id}", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path), ("user_id" = String, Path)), responses((status = 204, description = "Right removed: the instances already deployed are kept"), (status = 403, body = Message), (status = 404, body = Message)), tag = "Labs")]
pub async fn remove_user_permission(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id, user_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    remove_permission(&state, &user, &hypervisor_id, &lab_id, "user_id", &user_id).await
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/permissions/groups/{group_id}", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path), ("group_id" = String, Path)), responses((status = 204, description = "Right removed: the instances already deployed are kept"), (status = 403, body = Message), (status = 404, body = Message)), tag = "Labs")]
pub async fn remove_group_permission(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id, group_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    remove_permission(
        &state,
        &user,
        &hypervisor_id,
        &lab_id,
        "group_id",
        &group_id,
    )
    .await
}

/// `column`: `user_id` or `group_id`.
async fn set_permission(
    state: &SharedState,
    lab_id: &str,
    column: &str,
    principal_id: &str,
    body: &SetLabPermission,
) -> Result<(), ApiError> {
    if body.max_instances.is_some_and(|max| max < 0) {
        return Err(ApiError::BadRequest(
            "max_instances must be 0 or more (null: no limit)".into(),
        ));
    }
    let at = now();
    sqlx::query(&format!(
        "INSERT INTO lab_permissions (lab_id, {column}, role, max_instances, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $5)
         ON CONFLICT (lab_id, {column}) WHERE {column} IS NOT NULL
         DO UPDATE SET role = excluded.role, max_instances = excluded.max_instances, updated_at = excluded.updated_at"
    ))
    .bind(lab_id)
    .bind(principal_id)
    .bind(body.role.unwrap_or(TemplateRole::Use))
    .bind(body.max_instances)
    .bind(at)
    .execute(&state.db)
    .await?;
    Ok(())
}

async fn remove_permission(
    state: &SharedState,
    user: &AuthUser,
    hypervisor_id: &str,
    lab_id: &str,
    column: &str,
    principal_id: &str,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    let (row, _) = find_lab(state, user, hypervisor_id, lab_id).await?;
    let result = sqlx::query(&format!(
        "DELETE FROM lab_permissions WHERE lab_id = $1 AND {column} = $2"
    ))
    .bind(&row.id)
    .bind(principal_id)
    .execute(&state.db)
    .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("Permission not found".into()));
    }
    audit::record(
        state,
        user,
        "lab.permission_remove",
        &format!("{hypervisor_id}/{lab_id}"),
        &format!("{column} {principal_id}"),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

fn permission_detail(kind: &str, name: &str, body: &SetLabPermission) -> String {
    let role = body.role.unwrap_or(TemplateRole::Use);
    match body.max_instances {
        Some(max) => format!("{kind} {name}: {role:?}, at most {max} instances"),
        None => format!("{kind} {name}: {role:?}, no limit"),
    }
}

// ---------- instances ----------

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/instances", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path)), responses((status = 200, body = [LabInstance], description = "The caller's instances; every instance for those managing the lab"), (status = 404, body = Message)), tag = "Labs")]
pub async fn list_instances(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id)): Path<(String, String)>,
) -> Result<Json<Vec<LabInstance>>, ApiError> {
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    fail_interrupted(&state).await?;
    let (row, rights) = find_lab(&state, &user, &hypervisor_id, &lab_id).await?;
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM lab_instances WHERE lab_id = $1 AND ($2 OR owner_id = $3) ORDER BY created_at",
    )
    .bind(&row.id)
    .bind(rights.manages())
    .bind(&user.id)
    .fetch_all(&state.db)
    .await?;
    let mut instances = Vec::with_capacity(ids.len());
    for id in ids {
        instances.push(load_instance(&state, &uri, &row, &id).await?);
    }
    Ok(Json(instances))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/instances", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path)), request_body = CreateInstance, responses((status = 202, body = LabInstance, description = "Deployment started: follow it with GET .../instances/{instance_id} until it is ready or failed"), (status = 400, body = Message), (status = 403, body = Message, description = "Quota or max_instances reached"), (status = 404, body = Message), (status = 409, body = Message, description = "The lab cannot be deployed (no VM, template not built, network missing...)")), tag = "Labs")]
pub async fn create_instance(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id)): Path<(String, String)>,
    Json(body): Json<CreateInstance>,
) -> Result<Accepted<LabInstance>, ApiError> {
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    fail_interrupted(&state).await?;
    let (row, rights) = find_lab(&state, &user, &hypervisor_id, &lab_id).await?;
    let name = match &body.name {
        Some(name) => validate_name(name)?,
        None => row.name.clone(),
    };
    let plan = plan(&state, &row).await?;
    {
        // External networks must exist before anything is created.
        let (uri, externals) = (uri.clone(), plan.external_networks());
        blocking(move || {
            let conn = Connect::open(Some(&uri))?;
            for network in externals {
                let found =
                    Network::lookup_by_name(&conn, &network).map_err(|e| match e.code() {
                        ErrorNumber::NoNetwork => ApiError::Conflict(format!(
                            "The external network {network} is missing from the hypervisor"
                        )),
                        _ => e.into(),
                    })?;
                if !found.is_active()? {
                    return Err(ApiError::Conflict(format!(
                        "The external network {network} is not active"
                    )));
                }
            }
            Ok(())
        })
        .await?;
    }

    LazyLock::force(&STARTED_AT);
    let instance_id = Uuid::new_v4().to_string();
    let usage = plan.usage();
    // Under the lock, so that two requests cannot both pass the check of the last free slot.
    let job = {
        let _quota = state.quota_lock.lock().await;
        access::check_quota(&state, &user.id, usage).await?;
        if let (false, Some(max)) = (user.is_admin(), rights.max_instances)
            && instances_of(&state, &row.id, &user.id).await? >= max
        {
            return Err(ApiError::Forbidden(format!(
                "Lab limit reached: you may own at most {max} instances of this lab"
            )));
        }
        let at = now();
        sqlx::query(
            "INSERT INTO lab_instances (id, lab_id, owner_id, name, status, created_at, updated_at)
             VALUES ($1, $2, $3, $4, 'deploying', $5, $5)",
        )
        .bind(&instance_id)
        .bind(&row.id)
        .bind(&user.id)
        .bind(&name)
        .bind(at)
        .execute(&state.db)
        .await?;
        state.jobs.start("lab_instance", &user.id, usage)
    };
    audit::record(
        &state,
        &user,
        "lab.deploy",
        &format!("{hypervisor_id}/{lab_id}"),
        &format!("{name} ({instance_id})"),
    )
    .await;

    let deployment = Deployment {
        state: Arc::clone(&state),
        uri: uri.clone(),
        hypervisor_id: hypervisor_id.clone(),
        instance_id: instance_id.clone(),
        owner_id: user.id.clone(),
        plan,
        start: body.start.unwrap_or(false),
        job_id: job.id,
    };
    tokio::spawn(deployment.run());

    let instance = load_instance(&state, &uri, &row, &instance_id).await?;
    let location = format!(
        "{}/instances/{instance_id}",
        lab_path(&hypervisor_id, &row.id)
    );
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, location)],
        Json(instance),
    ))
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/instances/{instance_id}", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path), ("instance_id" = String, Path)), responses((status = 200, body = LabInstance), (status = 404, body = Message)), tag = "Labs")]
pub async fn get_instance(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id, instance_id)): Path<(String, String, String)>,
) -> Result<Json<LabInstance>, ApiError> {
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    fail_interrupted(&state).await?;
    let (row, rights) = find_lab(&state, &user, &hypervisor_id, &lab_id).await?;
    find_instance(&state, &user, rights, &row, &instance_id).await?;
    Ok(Json(load_instance(&state, &uri, &row, &instance_id).await?))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/instances/{instance_id}/start", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path), ("instance_id" = String, Path)), responses((status = 200, body = LabInstance, description = "Every VM started, routers first"), (status = 404, body = Message), (status = 409, body = Message, description = "The instance is not ready"), (status = 502, body = Message)), tag = "Labs")]
pub async fn start_instance(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id, instance_id)): Path<(String, String, String)>,
) -> Result<Json<LabInstance>, ApiError> {
    power(
        &state,
        &user,
        &hypervisor_id,
        &lab_id,
        &instance_id,
        Power::Start,
    )
    .await
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/instances/{instance_id}/stop", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path), ("instance_id" = String, Path), StopQuery), responses((status = 200, body = LabInstance, description = "Every VM asked to shut down (ACPI, routers last), or powered off with force=true"), (status = 404, body = Message), (status = 409, body = Message, description = "The instance is not ready"), (status = 502, body = Message)), tag = "Labs")]
pub async fn stop_instance(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id, instance_id)): Path<(String, String, String)>,
    Query(query): Query<StopQuery>,
) -> Result<Json<LabInstance>, ApiError> {
    let mode = if query.force.unwrap_or(false) {
        Power::ForceStop
    } else {
        Power::Stop
    };
    power(&state, &user, &hypervisor_id, &lab_id, &instance_id, mode).await
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/instances/{instance_id}", params(("hypervisor_id" = String, Path), ("lab_id" = String, Path), ("instance_id" = String, Path)), responses((status = 204, description = "VMs (powered off), their disks and the internal networks deleted"), (status = 404, body = Message), (status = 409, body = Message, description = "Still deploying"), (status = 502, body = Message)), tag = "Labs")]
pub async fn delete_instance(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, lab_id, instance_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    fail_interrupted(&state).await?;
    let (row, rights) = find_lab(&state, &user, &hypervisor_id, &lab_id).await?;
    let (name, status) = find_instance(&state, &user, rights, &row, &instance_id).await?;
    if status == InstanceStatus::Deploying {
        return Err(ApiError::Conflict(
            "The instance is still being deployed: wait for it to end".into(),
        ));
    }
    teardown(&state, &uri, &instance_id).await?;
    sqlx::query("DELETE FROM lab_instances WHERE id = $1")
        .bind(&instance_id)
        .execute(&state.db)
        .await?;
    audit::record(
        &state,
        &user,
        "lab.instance_delete",
        &format!("{hypervisor_id}/{lab_id}"),
        &format!("{name} ({instance_id})"),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/me/lab-instances", responses((status = 200, body = [MyInstance], description = "The caller's lab instances, on every hypervisor")), tag = "Labs")]
pub async fn my_instances(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<Vec<MyInstance>>, ApiError> {
    fail_interrupted(&state).await?;
    Ok(Json(
        sqlx::query_as::<_, MyInstance>(
            "SELECT i.id, i.name, i.status, i.lab_id, l.name AS lab_name, l.hypervisor_id, i.created_at
             FROM lab_instances i JOIN labs l ON l.id = i.lab_id
             WHERE i.owner_id = $1 ORDER BY i.created_at",
        )
        .bind(&user.id)
        .fetch_all(&state.db)
        .await?,
    ))
}

#[derive(Clone, Copy, PartialEq)]
enum Power {
    Start,
    Stop,
    ForceStop,
}

async fn power(
    state: &SharedState,
    user: &AuthUser,
    hypervisor_id: &str,
    lab_id: &str,
    instance_id: &str,
    mode: Power,
) -> Result<Json<LabInstance>, ApiError> {
    let uri = libvirt::hypervisor_uri(&state.db, hypervisor_id).await?;
    fail_interrupted(state).await?;
    let (row, rights) = find_lab(state, user, hypervisor_id, lab_id).await?;
    let (name, status) = find_instance(state, user, rights, &row, instance_id).await?;
    if status != InstanceStatus::Ready {
        return Err(ApiError::Conflict("The instance is not ready".into()));
    }
    // Routers first when starting, last when stopping: the others depend on them.
    let mut vms: Vec<(String, VmRole)> = sqlx::query_as(
        "SELECT iv.vm_id, v.role FROM lab_instance_vms iv JOIN lab_vms v ON v.id = iv.lab_vm_id
         WHERE iv.instance_id = $1 ORDER BY v.role = 'router' DESC, iv.name",
    )
    .bind(instance_id)
    .fetch_all(&state.db)
    .await?;
    if mode != Power::Start {
        vms.reverse();
    }
    let ids: Vec<String> = vms.into_iter().map(|(id, _)| id).collect();
    let target = uri.clone();
    blocking(move || {
        let conn = Connect::open(Some(&target))?;
        for id in &ids {
            let Ok(domain) = Domain::lookup_by_uuid_string(&conn, id) else {
                continue;
            };
            match (mode, domain.is_active()?) {
                (Power::Start, false) => {
                    domain.create()?;
                }
                (Power::Stop, true) => {
                    domain.shutdown()?;
                }
                (Power::ForceStop, true) => {
                    domain.destroy()?;
                }
                _ => {}
            }
        }
        Ok(())
    })
    .await?;
    let action = match mode {
        Power::Start => "lab.instance_start",
        Power::Stop => "lab.instance_stop",
        Power::ForceStop => "lab.instance_stop_force",
    };
    audit::record(
        state,
        user,
        action,
        &format!("{hypervisor_id}/{lab_id}"),
        &format!("{name} ({instance_id})"),
    )
    .await;
    Ok(Json(load_instance(state, &uri, &row, instance_id).await?))
}

// ---------- deployment ----------

/// What deploying the lab creates, read from its definition when deployment starts.
struct Plan {
    networks: Vec<LabNetwork>,
    /// Routers first.
    vms: Vec<PlannedVm>,
}

struct PlannedVm {
    lab_vm_id: String,
    name: String,
    memory_mb: u64,
    vcpus: u64,
    uefi: bool,
    /// Lab network ids, in NIC order.
    networks: Vec<String>,
    pool: String,
    volume: String,
    disk_gb: u64,
}

impl Plan {
    fn usage(&self) -> Usage {
        self.vms.iter().fold(Usage::default(), |total, vm| {
            total + Usage::one_vm(vm.vcpus, vm.memory_mb, vm.disk_gb)
        })
    }

    fn external_networks(&self) -> Vec<String> {
        self.networks
            .iter()
            .filter_map(|n| n.libvirt_network.clone())
            .collect()
    }
}

#[derive(FromRow)]
struct PlanRow {
    id: String,
    name: String,
    memory_mb: i64,
    vcpus: i64,
    firmware: String,
    template_name: String,
    allow_vm_creation: bool,
    wanted_version: Option<i64>,
    pool: Option<String>,
    volume: Option<String>,
    disk_size_gb: Option<i64>,
}

async fn plan(state: &SharedState, lab: &LabRow) -> Result<Plan, ApiError> {
    let rows = sqlx::query_as::<_, PlanRow>(
        "SELECT v.id, v.name, v.memory_mb, v.vcpus, v.firmware, t.name AS template_name,
                t.allow_vm_creation, v.version AS wanted_version, tv.pool, tv.volume, tv.disk_size_gb
         FROM lab_vms v
         JOIN templates t ON t.id = v.template_id
         LEFT JOIN template_versions tv ON tv.template_id = v.template_id AND tv.status = 'created'
              AND tv.version = COALESCE(v.version, (SELECT MAX(x.version) FROM template_versions x
                                                     WHERE x.template_id = v.template_id AND x.status = 'created'))
         WHERE v.lab_id = $1
         ORDER BY v.role = 'router' DESC, v.name",
    )
    .bind(&lab.id)
    .fetch_all(&state.db)
    .await?;
    if rows.is_empty() {
        return Err(ApiError::Conflict("The lab has no VM to deploy".into()));
    }
    let nics: Vec<(String, String)> = sqlx::query_as(
        "SELECT n.lab_vm_id, n.network_id FROM lab_vm_nics n JOIN lab_vms v ON v.id = n.lab_vm_id
         WHERE v.lab_id = $1 ORDER BY n.lab_vm_id, n.position",
    )
    .bind(&lab.id)
    .fetch_all(&state.db)
    .await?;
    let mut nics_of: HashMap<String, Vec<String>> = HashMap::new();
    for (vm, network) in nics {
        nics_of.entry(vm).or_default().push(network);
    }
    let mut vms = Vec::with_capacity(rows.len());
    for row in rows {
        if !row.allow_vm_creation {
            return Err(ApiError::Conflict(format!(
                "VM {}: creating VMs from the template {} is disabled",
                row.name, row.template_name
            )));
        }
        let (Some(pool), Some(volume)) = (row.pool, row.volume) else {
            return Err(ApiError::Conflict(match row.wanted_version {
                Some(version) => format!(
                    "VM {}: version {version} of the template {} is not built",
                    row.name, row.template_name
                ),
                None => format!(
                    "VM {}: the template {} has no built version yet",
                    row.name, row.template_name
                ),
            }));
        };
        vms.push(PlannedVm {
            networks: nics_of.remove(&row.id).unwrap_or_default(),
            lab_vm_id: row.id,
            name: row.name,
            memory_mb: row.memory_mb as u64,
            vcpus: row.vcpus as u64,
            uefi: row.firmware == "uefi",
            pool,
            volume,
            disk_gb: row.disk_size_gb.unwrap_or(0).max(0) as u64,
        });
    }
    let networks = sqlx::query_as::<_, LabNetwork>(
        "SELECT id, name, kind, libvirt_network FROM lab_networks WHERE lab_id = $1 ORDER BY name",
    )
    .bind(&lab.id)
    .fetch_all(&state.db)
    .await?;
    Ok(Plan { networks, vms })
}

/// One deployment, run in the background.
struct Deployment {
    state: SharedState,
    uri: String,
    hypervisor_id: String,
    instance_id: String,
    owner_id: String,
    plan: Plan,
    start: bool,
    job_id: String,
}

impl Deployment {
    async fn run(self) {
        let outcome = self.deploy().await;
        let (status, error) = match &outcome {
            Ok(()) => ("ready", None),
            Err(error) => {
                // Leave nothing behind: what was created is removed, the instance stays as
                // failed to show why.
                if let Err(cleanup) = teardown(&self.state, &self.uri, &self.instance_id).await {
                    eprintln!(
                        "Could not clean up a failed lab deployment: {}",
                        cleanup.message()
                    );
                }
                ("failed", Some(error.message().to_string()))
            }
        };
        if let Err(e) = sqlx::query(
            "UPDATE lab_instances SET status = $1, error = $2, updated_at = $3 WHERE id = $4",
        )
        .bind(status)
        .bind(&error)
        .bind(now())
        .bind(&self.instance_id)
        .execute(&self.state.db)
        .await
        {
            eprintln!("Could not record the end of a lab deployment: {e}");
        }
        self.state.jobs.end(&self.job_id, error);
    }

    async fn deploy(&self) -> Result<(), ApiError> {
        let short = &self.instance_id.replace('-', "")[..8];
        let mut libvirt_names: HashMap<String, String> = HashMap::new();
        for network in &self.plan.networks {
            let (libvirt_network, created) = match &network.libvirt_network {
                Some(external) => (external.clone(), false),
                None => {
                    let name = format!("lab-{short}-{}", network.name);
                    let (uri, xml) = (self.uri.clone(), isolated_network_xml(&name)?);
                    blocking(move || {
                        let conn = Connect::open(Some(&uri))?;
                        let network = Network::define_xml(&conn, &xml)?;
                        if let Err(e) = network.create() {
                            let _ = network.undefine();
                            return Err(e.into());
                        }
                        Ok(())
                    })
                    .await?;
                    (name, true)
                }
            };
            // Recorded at once: a failure later knows what to remove.
            sqlx::query(
                "INSERT INTO lab_instance_networks (instance_id, network_id, libvirt_network, created)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(&self.instance_id)
            .bind(&network.id)
            .bind(&libvirt_network)
            .bind(created)
            .execute(&self.state.db)
            .await?;
            libvirt_names.insert(network.id.clone(), libvirt_network);
        }

        let vnc_listen = vm_create::vnc_listen()?;
        let mut created = Vec::with_capacity(self.plan.vms.len());
        for vm in &self.plan.vms {
            let new_vm = NewVm {
                uri: self.uri.clone(),
                name: format!("{}-{short}", vm.name),
                memory_mb: vm.memory_mb,
                vcpus: vm.vcpus,
                uefi: vm.uefi,
                start: false,
                networks: vm
                    .networks
                    .iter()
                    .map(|id| libvirt_names[id].clone())
                    .collect(),
                pool: vm.pool.clone(),
                template_volume: vm.volume.clone(),
                vnc_listen: vnc_listen.clone(),
            };
            let (name, pool, volume) = (
                new_vm.name.clone(),
                new_vm.pool.clone(),
                new_vm.disk_volume(),
            );
            let vm_id = blocking(move || {
                check_new_vm(&new_vm)?;
                define_from_template(&new_vm)
            })
            .await?;
            let usage = Usage::one_vm(vm.vcpus, vm.memory_mb, vm.disk_gb);
            let recorded = self
                .record_vm(&vm.lab_vm_id, &vm_id, &name, &pool, &volume, usage)
                .await;
            if let Err(error) = recorded {
                // Not known to the database: remove it here, teardown would not see it.
                let (uri, id) = (self.uri.clone(), vm_id.clone());
                let _ = blocking(move || destroy_vm(&uri, &id, &pool, &volume)).await;
                return Err(error);
            }
            created.push(vm_id);
        }

        if self.start {
            // Routers come first in the plan.
            let uri = self.uri.clone();
            blocking(move || {
                let conn = Connect::open(Some(&uri))?;
                for id in &created {
                    Domain::lookup_by_uuid_string(&conn, id)?.create()?;
                }
                Ok(())
            })
            .await?;
        }
        Ok(())
    }

    async fn record_vm(
        &self,
        lab_vm_id: &str,
        vm_id: &str,
        name: &str,
        pool: &str,
        volume: &str,
        usage: Usage,
    ) -> Result<(), ApiError> {
        access::record_owner(
            &self.state,
            &self.hypervisor_id,
            vm_id,
            &self.owner_id,
            usage,
        )
        .await?;
        sqlx::query(
            "INSERT INTO lab_instance_vms (instance_id, lab_vm_id, hypervisor_id, vm_id, name, pool, volume)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&self.instance_id)
        .bind(lab_vm_id)
        .bind(&self.hypervisor_id)
        .bind(access::parse_vm_id(vm_id)?)
        .bind(name)
        .bind(pool)
        .bind(volume)
        .execute(&self.state.db)
        .await?;
        Ok(())
    }
}

/// An isolated libvirt network: no forwarding, no address. The routers of the lab give the
/// addresses (DHCP of OPNsense...).
fn isolated_network_xml(name: &str) -> Result<String, ApiError> {
    let mut root = Element::new("network");
    let mut name_element = Element::new("name");
    name_element.children.push(XMLNode::Text(name.to_owned()));
    root.children.push(XMLNode::Element(name_element));
    let mut xml = Vec::new();
    root.write(&mut xml)
        .map_err(|e| ApiError::Internal(format!("Could not write the network XML: {e}")))?;
    String::from_utf8(xml).map_err(|e| ApiError::Internal(format!("Invalid network XML: {e}")))
}

/// Removes what an instance has on the hypervisor: its VMs (powered off), their disks and its
/// internal networks, then their rows. Missing things are skipped: it can be run again.
async fn teardown(state: &SharedState, uri: &str, instance_id: &str) -> Result<(), ApiError> {
    let vms: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT hypervisor_id, vm_id, pool, volume FROM lab_instance_vms WHERE instance_id = $1",
    )
    .bind(instance_id)
    .fetch_all(&state.db)
    .await?;
    for (hypervisor_id, vm_id, pool, volume) in vms {
        let (target, id) = (uri.to_owned(), vm_id.clone());
        blocking(move || destroy_vm(&target, &id, &pool, &volume)).await?;
        // Also removes the lab_instance_vms row and the shares of the VM.
        access::forget_vm(state, &hypervisor_id, &vm_id).await;
    }
    let networks: Vec<String> = sqlx::query_scalar(
        "SELECT libvirt_network FROM lab_instance_networks WHERE instance_id = $1 AND created",
    )
    .bind(instance_id)
    .fetch_all(&state.db)
    .await?;
    let target = uri.to_owned();
    blocking(move || {
        let conn = Connect::open(Some(&target))?;
        for name in &networks {
            let network = match Network::lookup_by_name(&conn, name) {
                Ok(network) => network,
                Err(e) if e.code() == ErrorNumber::NoNetwork => continue,
                Err(e) => return Err(e.into()),
            };
            if network.is_active()? {
                network.destroy()?;
            }
            network.undefine()?;
        }
        Ok(())
    })
    .await?;
    sqlx::query("DELETE FROM lab_instance_networks WHERE instance_id = $1")
        .bind(instance_id)
        .execute(&state.db)
        .await?;
    Ok(())
}

/// Powers a VM off, removes it and its disk. A VM or disk already gone is skipped.
fn destroy_vm(uri: &str, vm_id: &str, pool: &str, volume: &str) -> Result<(), ApiError> {
    let conn = Connect::open(Some(uri))?;
    match Domain::lookup_by_uuid_string(&conn, vm_id) {
        Ok(domain) => {
            if domain.is_active()? {
                domain.destroy()?;
            }
            // libvirt refuses to undefine a VM that has an NVRAM file unless asked to remove
            // it, and some drivers refuse the flag otherwise.
            if vm_xml::has_nvram(&domain.get_xml_desc(0)?)? {
                domain.undefine_flags(sys::VIR_DOMAIN_UNDEFINE_NVRAM)?;
            } else {
                domain.undefine()?;
            }
        }
        Err(e) if e.code() == ErrorNumber::NoDomain => {}
        Err(e) => return Err(e.into()),
    }
    templates::delete_volume(uri, pool, volume)
}

/// Instances left `deploying` by a previous run of the API will never end: they failed.
async fn fail_interrupted(state: &SharedState) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE lab_instances SET status = 'failed', error = $1, updated_at = $2
         WHERE status = 'deploying' AND created_at < $3",
    )
    .bind(INTERRUPTED)
    .bind(now())
    .bind(*STARTED_AT)
    .execute(&state.db)
    .await?;
    Ok(())
}

// ---------- lookups and views ----------

async fn find_lab(
    state: &SharedState,
    user: &AuthUser,
    hypervisor_id: &str,
    lab_id: &str,
) -> Result<(LabRow, Rights), ApiError> {
    let not_found = || ApiError::NotFound("Lab not found".into());
    let row = sqlx::query_as::<_, LabRow>(&format!(
        "SELECT {LAB_COLUMNS} FROM labs WHERE hypervisor_id = $1 AND id = $2"
    ))
    .bind(hypervisor_id)
    .bind(lab_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(not_found)?;
    // Someone with no right on the lab gets a 404, as if it did not exist.
    let rights = Caller::of(state, user, hypervisor_id)
        .await?
        .rights(state, &row)
        .await?
        .ok_or_else(not_found)?;
    Ok((row, rights))
}

/// The name and status of an instance the caller may act on: their own, or any for those
/// managing the lab.
async fn find_instance(
    state: &SharedState,
    user: &AuthUser,
    rights: Rights,
    lab: &LabRow,
    instance_id: &str,
) -> Result<(String, InstanceStatus), ApiError> {
    let found: Option<(String, InstanceStatus)> = sqlx::query_as(
        "SELECT name, status FROM lab_instances
         WHERE id = $1 AND lab_id = $2 AND ($3 OR owner_id = $4)",
    )
    .bind(instance_id)
    .bind(&lab.id)
    .bind(rights.manages())
    .bind(&user.id)
    .fetch_optional(&state.db)
    .await?;
    found.ok_or_else(|| ApiError::NotFound("Instance not found".into()))
}

async fn load_instance(
    state: &SharedState,
    uri: &str,
    lab: &LabRow,
    instance_id: &str,
) -> Result<LabInstance, ApiError> {
    let (id, owner_id, owner, name, status, error, created_at): (
        String,
        String,
        String,
        String,
        InstanceStatus,
        Option<String>,
        i64,
    ) = sqlx::query_as(
        "SELECT i.id, i.owner_id, u.username, i.name, i.status, i.error, i.created_at
         FROM lab_instances i JOIN users u ON u.id = i.owner_id WHERE i.id = $1",
    )
    .bind(instance_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::NotFound("Instance not found".into()))?;
    let networks = sqlx::query_as::<_, InstanceNetwork>(
        "SELECT n.network_id, ln.name, ln.kind, n.libvirt_network
         FROM lab_instance_networks n JOIN lab_networks ln ON ln.id = n.network_id
         WHERE n.instance_id = $1 ORDER BY ln.name",
    )
    .bind(instance_id)
    .fetch_all(&state.db)
    .await?;
    let rows: Vec<(String, String, String, VmRole)> = sqlx::query_as(
        "SELECT iv.lab_vm_id, iv.vm_id, iv.name, v.role
         FROM lab_instance_vms iv JOIN lab_vms v ON v.id = iv.lab_vm_id
         WHERE iv.instance_id = $1 ORDER BY v.role = 'router' DESC, iv.name",
    )
    .bind(instance_id)
    .fetch_all(&state.db)
    .await?;
    let ids: Vec<String> = rows.iter().map(|row| row.1.clone()).collect();
    let target = uri.to_owned();
    let live = blocking(move || live_vms(&target, &ids)).await?;
    let vms = rows
        .into_iter()
        .map(|(lab_vm_id, vm_id, name, role)| {
            let (status, networks) = live
                .get(&vm_id)
                .cloned()
                .unwrap_or_else(|| ("missing".into(), Vec::new()));
            InstanceVm {
                lab_vm_id,
                vm_id,
                name,
                role,
                status,
                networks,
            }
        })
        .collect();
    Ok(LabInstance {
        id,
        lab_id: lab.id.clone(),
        lab_name: lab.name.clone(),
        hypervisor_id: lab.hypervisor_id.clone(),
        owner_id,
        owner,
        name,
        status,
        error,
        created_at,
        networks,
        vms,
    })
}

/// State and NIC networks of each VM still on the hypervisor, by UUID.
fn live_vms(uri: &str, ids: &[String]) -> Result<HashMap<String, (String, Vec<String>)>, ApiError> {
    let mut live = HashMap::new();
    if ids.is_empty() {
        return Ok(live);
    }
    let conn = Connect::open(Some(uri))?;
    for id in ids {
        let Ok(domain) = Domain::lookup_by_uuid_string(&conn, id) else {
            continue;
        };
        let status = libvirt::state_label(domain.get_info()?.state).to_owned();
        let networks = vm_xml::interface_networks(&domain.get_xml_desc(0)?)?;
        live.insert(id.clone(), (status, networks));
    }
    Ok(live)
}

async fn view(
    state: &SharedState,
    user: &AuthUser,
    row: LabRow,
    rights: Rights,
) -> Result<Lab, ApiError> {
    Ok(Lab {
        access: LabAccess {
            level: rights.level,
            max_instances: rights.max_instances,
            instance_count: instances_of(state, &row.id, &user.id).await?,
        },
        id: row.id,
        hypervisor_id: row.hypervisor_id,
        name: row.name,
        description: row.description,
        visibility: row.visibility,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

async fn detail(
    state: &SharedState,
    user: &AuthUser,
    row: LabRow,
    rights: Rights,
) -> Result<LabDetail, ApiError> {
    let networks = sqlx::query_as::<_, LabNetwork>(
        "SELECT id, name, kind, libvirt_network FROM lab_networks WHERE lab_id = $1 ORDER BY name",
    )
    .bind(&row.id)
    .fetch_all(&state.db)
    .await?;
    let vms = load_vms(state, &row.id, None).await?;
    Ok(LabDetail {
        lab: view(state, user, row, rights).await?,
        networks,
        vms,
    })
}

/// The VMs of a lab (or one of them), with their NICs.
async fn load_vms(
    state: &SharedState,
    lab_id: &str,
    only: Option<&str>,
) -> Result<Vec<LabVm>, ApiError> {
    let rows = sqlx::query_as::<_, LabVmRow>(
        "SELECT v.id, v.name, v.role, v.template_id, t.name AS template_name, v.version,
                v.memory_mb, v.vcpus, v.firmware
         FROM lab_vms v JOIN templates t ON t.id = v.template_id
         WHERE v.lab_id = $1 AND ($2::TEXT IS NULL OR v.id = $2)
         ORDER BY v.role = 'router' DESC, v.name",
    )
    .bind(lab_id)
    .bind(only)
    .fetch_all(&state.db)
    .await?;
    let nics: Vec<(String, String)> = sqlx::query_as(
        "SELECT n.lab_vm_id, n.network_id FROM lab_vm_nics n JOIN lab_vms v ON v.id = n.lab_vm_id
         WHERE v.lab_id = $1 ORDER BY n.lab_vm_id, n.position",
    )
    .bind(lab_id)
    .fetch_all(&state.db)
    .await?;
    let mut nics_of: HashMap<String, Vec<String>> = HashMap::new();
    for (vm, network) in nics {
        nics_of.entry(vm).or_default().push(network);
    }
    Ok(rows
        .into_iter()
        .map(|row| LabVm {
            networks: nics_of.remove(&row.id).unwrap_or_default(),
            id: row.id,
            name: row.name,
            role: row.role,
            template_id: row.template_id,
            template_name: row.template_name,
            version: row.version,
            memory_mb: row.memory_mb,
            vcpus: row.vcpus,
            firmware: row.firmware,
        })
        .collect())
}

async fn load_vm(state: &SharedState, lab_id: &str, lab_vm_id: &str) -> Result<LabVm, ApiError> {
    load_vms(state, lab_id, Some(lab_vm_id))
        .await?
        .pop()
        .ok_or_else(|| ApiError::NotFound("Lab VM not found".into()))
}

/// Instances of the lab owned by `owner_id`, whatever their state.
async fn instances_of(state: &SharedState, lab_id: &str, owner_id: &str) -> Result<i64, ApiError> {
    Ok(
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM lab_instances WHERE lab_id = $1 AND owner_id = $2",
        )
        .bind(lab_id)
        .bind(owner_id)
        .fetch_one(&state.db)
        .await?,
    )
}

/// `table.column` must not reference `id` (a lab VM or network an instance was made with).
async fn ensure_unused_by_instances(
    state: &SharedState,
    table: &str,
    column: &str,
    id: &str,
) -> Result<(), ApiError> {
    let used: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(DISTINCT instance_id) FROM {table} WHERE {column} = $1"
    ))
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    if used > 0 {
        return Err(ApiError::Conflict(format!(
            "{used} instance(s) of the lab were deployed with it: delete them first"
        )));
    }
    Ok(())
}

async fn touch(state: &SharedState, lab_id: &str) -> Result<(), ApiError> {
    sqlx::query("UPDATE labs SET updated_at = $1 WHERE id = $2")
        .bind(now())
        .bind(lab_id)
        .execute(&state.db)
        .await?;
    Ok(())
}

// ---------- rights ----------

/// What a caller may do with one lab: every right that applies to them, combined.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rights {
    level: AccessLevel,
    /// None: no limit.
    max_instances: Option<i64>,
}

impl Rights {
    /// Adds a right: the highest level and the most generous limit win.
    fn with(self, level: AccessLevel, max_instances: Option<i64>) -> Rights {
        Rights {
            level: self.level.max(level),
            max_instances: match (self.max_instances, max_instances) {
                (Some(a), Some(b)) => Some(a.max(b)),
                _ => None,
            },
        }
    }

    fn manages(self) -> bool {
        self.level >= AccessLevel::Manage
    }

    fn require_manage(self) -> Result<(), ApiError> {
        if self.manages() {
            Ok(())
        } else {
            Err(ApiError::Forbidden("You do not manage this lab".into()))
        }
    }
}

fn level_of(role: TemplateRole) -> AccessLevel {
    match role {
        TemplateRole::Use => AccessLevel::Use,
        TemplateRole::Manage => AccessLevel::Manage,
    }
}

/// One right on a lab: given to a user, or to a group.
#[derive(FromRow)]
struct RightRow {
    user_id: Option<String>,
    group_id: Option<String>,
    role: TemplateRole,
    max_instances: Option<i64>,
}

/// The caller, with what decides their rights on the labs of one hypervisor.
struct Caller<'a> {
    user: &'a AuthUser,
    /// The groups they are in, nested ones included.
    groups: Vec<String>,
    /// Granted the hypervisor: its `hypervisor` labs are open to them.
    granted: bool,
}

impl<'a> Caller<'a> {
    async fn of(
        state: &SharedState,
        user: &'a AuthUser,
        hypervisor_id: &str,
    ) -> Result<Caller<'a>, ApiError> {
        if user.is_admin() {
            return Ok(Caller {
                user,
                groups: Vec::new(),
                granted: true,
            });
        }
        let groups = groups::groups_of(state, &user.id)
            .await?
            .into_iter()
            .map(|group| group.id)
            .collect();
        let granted = match access::require_grant(state, user, hypervisor_id).await {
            Ok(()) => true,
            Err(ApiError::Forbidden(_)) => false,
            Err(error) => return Err(error),
        };
        Ok(Caller {
            user,
            groups,
            granted,
        })
    }

    /// None when the caller may not even see the lab.
    async fn rights(&self, state: &SharedState, lab: &LabRow) -> Result<Option<Rights>, ApiError> {
        if self.user.is_admin() {
            return Ok(Some(Rights {
                level: AccessLevel::Admin,
                max_instances: None,
            }));
        }
        let entries = sqlx::query_as::<_, RightRow>(
            "SELECT user_id, group_id, role, max_instances FROM lab_permissions WHERE lab_id = $1",
        )
        .bind(&lab.id)
        .fetch_all(&state.db)
        .await?;
        let mut rights: Option<Rights> = None;
        let mut add = |level: AccessLevel, max_instances: Option<i64>| {
            rights = Some(match rights {
                Some(rights) => rights.with(level, max_instances),
                None => Rights {
                    level,
                    max_instances,
                },
            });
        };
        for entry in entries {
            let applies = entry.user_id.as_deref() == Some(self.user.id.as_str())
                || entry
                    .group_id
                    .is_some_and(|group| self.groups.contains(&group));
            if applies {
                add(level_of(entry.role), entry.max_instances);
            }
        }
        if lab.visibility == Visibility::Hypervisor && self.granted {
            add(AccessLevel::Use, None);
        }
        Ok(rights)
    }
}

// ---------- validation ----------

fn validate_name(name: &str) -> Result<String, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_LEN || name.chars().any(char::is_control)
    {
        return Err(ApiError::BadRequest(format!(
            "name must be 1 to {MAX_NAME_LEN} printable characters"
        )));
    }
    Ok(name.to_owned())
}

fn validate_description(description: &str) -> Result<String, ApiError> {
    let description = description.trim();
    if description.chars().count() > MAX_DESCRIPTION_LEN
        || description
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(ApiError::BadRequest(format!(
            "description must be at most {MAX_DESCRIPTION_LEN} printable characters"
        )));
    }
    Ok(description.to_owned())
}

fn validate_network_name(name: &str) -> Result<String, ApiError> {
    let valid = !name.is_empty()
        && name.len() <= MAX_NETWORK_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(name.to_owned())
    } else {
        Err(ApiError::BadRequest(format!(
            "A network name is 1 to {MAX_NETWORK_NAME_LEN} letters, digits, '-' or '_'"
        )))
    }
}

fn in_range(field: &str, value: u64, min: u64, max: u64) -> Result<(), ApiError> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!(
            "{field} must be between {min} and {max}"
        )))
    }
}

fn conflict(error: sqlx::Error, message: &str) -> ApiError {
    match error {
        sqlx::Error::Database(e) if e.is_unique_violation() => ApiError::Conflict(message.into()),
        other => other.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::http::Method;
    use serde_json::{Value, json};
    use virt::{storage_pool::StoragePool, storage_vol::StorageVol};

    use super::*;
    use crate::http_tests::{POOL, TestApp, connect, create_volume, unique};

    fn labs(app: &TestApp) -> String {
        format!("/api/hypervisors/{}/labs", app.hypervisor_id)
    }

    /// A template with a built version 1. `with_disk: false`: the database says it is built,
    /// but its disk is missing (a deployment then fails).
    async fn built_template(app: &TestApp, with_disk: bool) -> String {
        let name = unique("tpl");
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO templates (id, hypervisor_id, name, packer_config, disk_name, pool, created_at, updated_at)
             VALUES ($1, $2, $3, 'x', 'disk.qcow2', $4, 0, 0)",
        )
        .bind(&id)
        .bind(&app.hypervisor_id)
        .bind(&name)
        .bind(POOL)
        .execute(&app.state.db)
        .await
        .unwrap();
        let volume = format!("template-{name}-v1.qcow2");
        if with_disk {
            create_volume(&volume);
        }
        sqlx::query(
            "INSERT INTO template_versions (template_id, version, packer_config, disk_name, pool, volume, status, disk_size_gb, created_at)
             VALUES ($1, 1, 'x', 'disk.qcow2', $2, $3, 'created', 1, $4)",
        )
        .bind(&id)
        .bind(POOL)
        .bind(&volume)
        .bind(now())
        .execute(&app.state.db)
        .await
        .unwrap();
        id
    }

    async fn post(app: &TestApp, path: &str, body: Value) -> Value {
        let (status, _, json) = app.send(Method::POST, path, Some(body)).await;
        assert!(status.is_success(), "POST {path}: {status} {json}");
        json
    }

    /// wan (external, libvirt `default`), lan1 and lan2 (internal); an OPNsense-like router on
    /// all three, and one client on each LAN. Returns the lab path and its network ids.
    async fn router_lab(app: &TestApp, template: &str) -> (String, Value) {
        let lab = post(app, &labs(app), json!({ "name": unique("lab") })).await;
        let path = format!("{}/{}", labs(app), lab["id"].as_str().unwrap());
        let wan = post(
            app,
            &format!("{path}/networks"),
            json!({ "name": "wan", "kind": "external", "libvirt_network": "default" }),
        )
        .await;
        let lan1 = post(
            app,
            &format!("{path}/networks"),
            json!({ "name": "lan1", "kind": "internal" }),
        )
        .await;
        let lan2 = post(
            app,
            &format!("{path}/networks"),
            json!({ "name": "lan2", "kind": "internal" }),
        )
        .await;
        let ids = json!({ "wan": wan["id"], "lan1": lan1["id"], "lan2": lan2["id"] });
        post(
            app,
            &format!("{path}/vms"),
            json!({
                "name": "opnsense", "role": "router", "template_id": template,
                "memory_mb": 1024, "vcpus": 1, "networks": [ids["wan"], ids["lan1"], ids["lan2"]]
            }),
        )
        .await;
        post(
            app,
            &format!("{path}/vms"),
            json!({
                "name": "client1", "template_id": template, "memory_mb": 512, "vcpus": 1,
                "networks": [ids["lan1"]]
            }),
        )
        .await;
        post(
            app,
            &format!("{path}/vms"),
            json!({
                "name": "client2", "template_id": template, "memory_mb": 512, "vcpus": 1,
                "networks": [ids["lan2"]]
            }),
        )
        .await;
        (path, ids)
    }

    /// Polls an instance until its deployment ended.
    async fn wait_instance(app: &TestApp, token: &str, path: &str) -> Value {
        for _ in 0..300 {
            let (status, json) = app.status_as(token, Method::GET, path).await;
            assert_eq!(status, StatusCode::OK, "{json}");
            if json["status"] != "deploying" {
                return json;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("{path} never finished deploying");
    }

    async fn deploy(app: &TestApp, token: &str, lab: &str, start: bool) -> (StatusCode, Value) {
        let (status, _, json) = app
            .send_as(
                Some(token),
                Method::POST,
                &format!("{lab}/instances"),
                Some(json!({ "start": start })),
            )
            .await;
        (status, json)
    }

    fn network_exists(name: &str) -> bool {
        Network::lookup_by_name(&connect(), name).is_ok()
    }

    fn volume_exists(name: &str) -> bool {
        let pool = StoragePool::lookup_by_name(&connect(), POOL).unwrap();
        StorageVol::lookup_by_name(&pool, name).is_ok()
    }

    #[test]
    fn names_are_checked() {
        assert!(validate_network_name("lan_1-a").is_ok());
        assert!(validate_network_name("").is_err());
        assert!(validate_network_name("with space").is_err());
        assert!(validate_network_name(&"x".repeat(MAX_NETWORK_NAME_LEN + 1)).is_err());
        assert!(validate_name("  Lab réseau  ").is_ok_and(|n| n == "Lab réseau"));
        assert!(validate_name("a\nb").is_err());
    }

    #[tokio::test]
    async fn a_lab_is_deployed_wired_driven_and_deleted() {
        let app = TestApp::new().await;
        let template = built_template(&app, true).await;
        let (lab, _) = router_lab(&app, &template).await;

        let (status, detail) = app.status_of(Method::GET, &lab).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["networks"].as_array().unwrap().len(), 3);
        assert_eq!(detail["vms"][0]["name"], "opnsense", "routers first");
        assert_eq!(detail["vms"][0]["networks"].as_array().unwrap().len(), 3);

        let (status, instance) = deploy(&app, &app.admin_token, &lab, true).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{instance}");
        let path = format!("{lab}/instances/{}", instance["id"].as_str().unwrap());
        let instance = wait_instance(&app, &app.admin_token, &path).await;
        assert_eq!(instance["status"], "ready", "{instance}");
        let vms = instance["vms"].as_array().unwrap();
        assert_eq!(vms.len(), 3);
        let router = &vms[0];
        assert_eq!(router["role"], "router");
        assert_eq!(router["status"], "running");
        // The NICs keep their order: WAN (external) first, then the instance's own LANs.
        let nics: Vec<&str> = router["networks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        assert_eq!(nics.len(), 3);
        assert_eq!(nics[0], "default");
        assert!(
            nics[1].starts_with("lab-") && nics[1].ends_with("-lan1"),
            "{nics:?}"
        );
        assert!(nics[2].ends_with("-lan2"));
        let client1 = vms
            .iter()
            .find(|vm| vm["name"].as_str().unwrap().starts_with("client1"))
            .unwrap();
        assert_eq!(
            client1["networks"][0], nics[1],
            "client1 shares lan1 with the router"
        );
        assert!(network_exists(nics[1]) && network_exists(nics[2]));

        // The VMs are ordinary VMs of their owner: the VM routes work on them.
        let vm_path = format!("{}/{}", app.vms(), router["vm_id"].as_str().unwrap());
        let (status, vm) = app.status_of(Method::GET, &vm_path).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(vm["access"], "owner");

        let (status, stopped) = app
            .status_of(Method::POST, &format!("{path}/stop?force=true"))
            .await;
        assert_eq!(status, StatusCode::OK, "{stopped}");
        assert!(
            stopped["vms"]
                .as_array()
                .unwrap()
                .iter()
                .all(|vm| vm["status"] == "stopped")
        );
        let (status, started) = app.status_of(Method::POST, &format!("{path}/start")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            started["vms"]
                .as_array()
                .unwrap()
                .iter()
                .all(|vm| vm["status"] == "running")
        );

        let (_, mine) = app.status_of(Method::GET, "/api/me/lab-instances").await;
        assert!(
            mine.as_array()
                .unwrap()
                .iter()
                .any(|i| i["id"] == instance["id"])
        );

        let disks: Vec<String> = vms
            .iter()
            .map(|vm| format!("{}.qcow2", vm["name"].as_str().unwrap()))
            .collect();
        let (status, deleted) = app.status_of(Method::DELETE, &path).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{deleted}");
        assert!(!network_exists(nics[1]) && !network_exists(nics[2]));
        assert!(
            network_exists("default"),
            "the external network is shared: kept"
        );
        assert!(disks.iter().all(|disk| !volume_exists(disk)), "{disks:?}");
        assert!(
            Domain::lookup_by_uuid_string(&connect(), router["vm_id"].as_str().unwrap()).is_err()
        );
        let (status, _) = app.status_of(Method::GET, &path).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn rights_and_limits_follow_the_template_model() {
        let app = TestApp::new().await;
        let template = built_template(&app, true).await;
        let lab = post(&app, &labs(&app), json!({ "name": unique("lab") })).await;
        let lab_id = lab["id"].as_str().unwrap();
        let path = format!("{}/{lab_id}", labs(&app));
        post(
            &app,
            &format!("{path}/vms"),
            json!({ "name": "solo", "template_id": template, "memory_mb": 256, "vcpus": 1 }),
        )
        .await;

        let (alice, alice_token) = app.user(&unique("alice"), "user").await;
        let (bob, bob_token) = app.user(&unique("bob"), "user").await;
        let (teacher, teacher_token) = app.user(&unique("teacher"), "user").await;
        // Restricted by default: a user with no right does not see it.
        let (status, _) = app.status_as(&alice_token, Method::GET, &path).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (_, listed) = app.status_as(&alice_token, Method::GET, &labs(&app)).await;
        assert!(listed.as_array().unwrap().is_empty());

        // A group with alice and bob may use it, one instance each.
        let (_, _, group) = app
            .send(
                Method::POST,
                "/api/groups",
                Some(json!({ "name": unique("class") })),
            )
            .await;
        let group = group["id"].as_str().unwrap();
        for member in [&alice, &bob] {
            app.status_of(
                Method::PUT,
                &format!("/api/groups/{group}/members/{member}"),
            )
            .await;
        }
        let (status, _, _) = app
            .send(
                Method::PUT,
                &format!("{path}/permissions/groups/{group}"),
                Some(json!({ "max_instances": 1 })),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _, _) = app
            .send(
                Method::PUT,
                &format!("{path}/permissions/users/{teacher}"),
                Some(json!({ "role": "manage" })),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (_, seen) = app.status_as(&alice_token, Method::GET, &path).await;
        assert_eq!(seen["access"]["level"], "use");
        assert_eq!(seen["access"]["max_instances"], 1);
        let (status, first) = deploy(&app, &alice_token, &path, false).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{first}");
        let alice_instance = format!("{path}/instances/{}", first["id"].as_str().unwrap());
        assert_eq!(
            wait_instance(&app, &alice_token, &alice_instance).await["status"],
            "ready"
        );
        let (status, refused) = deploy(&app, &alice_token, &path, false).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
        let (status, second) = deploy(&app, &bob_token, &path, false).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let bob_instance = format!("{path}/instances/{}", second["id"].as_str().unwrap());
        wait_instance(&app, &bob_token, &bob_instance).await;

        // Users see and drive their own instances only, and cannot edit the lab.
        let (_, alice_list) = app
            .status_as(&alice_token, Method::GET, &format!("{path}/instances"))
            .await;
        assert_eq!(alice_list.as_array().unwrap().len(), 1);
        let (status, _) = app
            .status_as(&alice_token, Method::GET, &bob_instance)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = app
            .status_as(&alice_token, Method::DELETE, &bob_instance)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        for (method, target, body) in [
            (
                Method::POST,
                format!("{path}/vms"),
                Some(json!({ "name": "x", "template_id": template, "memory_mb": 256, "vcpus": 1 })),
            ),
            (
                Method::POST,
                format!("{path}/networks"),
                Some(json!({ "name": "x", "kind": "internal" })),
            ),
            (Method::GET, format!("{path}/permissions"), None),
            (Method::DELETE, path.clone(), None),
        ] {
            let (status, _, _) = app
                .send_as(Some(&alice_token), method.clone(), &target, body)
                .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {target}");
        }

        // A manager sees every instance and may delete them, but does not give rights.
        let (_, all) = app
            .status_as(&teacher_token, Method::GET, &format!("{path}/instances"))
            .await;
        assert_eq!(all.as_array().unwrap().len(), 2);
        let (status, _) = app
            .status_as(&teacher_token, Method::DELETE, &bob_instance)
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _, _) = app
            .send_as(
                Some(&teacher_token),
                Method::PATCH,
                &path,
                Some(json!({ "visibility": "hypervisor" })),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = app
            .status_as(&teacher_token, Method::GET, &format!("{path}/permissions"))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // The permission list counts the instances of each user.
        let (_, permissions) = app
            .status_of(Method::GET, &format!("{path}/permissions"))
            .await;
        assert_eq!(permissions[0]["kind"], "user");
        assert_eq!(permissions[0]["instance_count"], 0);

        let (status, _) = app
            .status_as(&alice_token, Method::DELETE, &alice_instance)
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn invalid_definitions_are_refused() {
        let app = TestApp::new().await;
        let template = built_template(&app, true).await;
        let lab = post(&app, &labs(&app), json!({ "name": unique("lab") })).await;
        let path = format!("{}/{}", labs(&app), lab["id"].as_str().unwrap());
        let other = post(&app, &labs(&app), json!({ "name": unique("lab") })).await;
        let other_net = post(
            &app,
            &format!("{}/{}/networks", labs(&app), other["id"].as_str().unwrap()),
            json!({ "name": "lan", "kind": "internal" }),
        )
        .await;

        for (body, expected) in [
            (
                json!({ "name": "wan", "kind": "external" }),
                StatusCode::BAD_REQUEST,
            ),
            (
                json!({ "name": "wan", "kind": "external", "libvirt_network": "nope-net" }),
                StatusCode::NOT_FOUND,
            ),
            (
                json!({ "name": "lan", "kind": "internal", "libvirt_network": "default" }),
                StatusCode::BAD_REQUEST,
            ),
            (
                json!({ "name": "bad name", "kind": "internal" }),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let (status, _, json) = app
                .send(
                    Method::POST,
                    &format!("{path}/networks"),
                    Some(body.clone()),
                )
                .await;
            assert_eq!(status, expected, "{body} {json}");
        }
        for (body, expected) in [
            (
                json!({ "name": "vm", "template_id": template, "memory_mb": 256, "vcpus": 1, "networks": [other_net["id"]] }),
                StatusCode::NOT_FOUND,
            ),
            (
                json!({ "name": "vm", "template_id": Uuid::new_v4().to_string(), "memory_mb": 256, "vcpus": 1 }),
                StatusCode::NOT_FOUND,
            ),
            (
                json!({ "name": "vm", "template_id": template, "version": 9, "memory_mb": 256, "vcpus": 1 }),
                StatusCode::NOT_FOUND,
            ),
            (
                json!({ "name": "vm", "template_id": template, "memory_mb": 1, "vcpus": 1 }),
                StatusCode::BAD_REQUEST,
            ),
            (
                json!({ "name": "bad name", "template_id": template, "memory_mb": 256, "vcpus": 1 }),
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let (status, _, json) = app
                .send(Method::POST, &format!("{path}/vms"), Some(body.clone()))
                .await;
            assert_eq!(status, expected, "{body} {json}");
        }

        // Nothing to deploy yet.
        let (status, _) = deploy(&app, &app.admin_token, &path, false).await;
        assert_eq!(status, StatusCode::CONFLICT);

        let lan = post(
            &app,
            &format!("{path}/networks"),
            json!({ "name": "lan", "kind": "internal" }),
        )
        .await;
        let vm = post(&app, &format!("{path}/vms"), json!({ "name": "vm", "template_id": template, "memory_mb": 256, "vcpus": 1, "networks": [lan["id"]] })).await;
        let lan_path = format!("{path}/networks/{}", lan["id"].as_str().unwrap());
        let (status, _) = app.status_of(Method::DELETE, &lan_path).await;
        assert_eq!(status, StatusCode::CONFLICT, "a VM is plugged into it");
        let (status, _) = app
            .status_of(
                Method::DELETE,
                &format!(
                    "/api/hypervisors/{}/templates/{template}",
                    app.hypervisor_id
                ),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "the lab uses the template");

        let (status, instance) = deploy(&app, &app.admin_token, &path, false).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let instance_path = format!("{path}/instances/{}", instance["id"].as_str().unwrap());
        wait_instance(&app, &app.admin_token, &instance_path).await;
        let (status, _) = app
            .status_of(
                Method::DELETE,
                &format!("{path}/vms/{}", vm["id"].as_str().unwrap()),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "an instance has it");
        let (status, _) = app.status_of(Method::DELETE, &path).await;
        assert_eq!(status, StatusCode::CONFLICT, "the lab has an instance");
        let hypervisor = format!("/api/hypervisors/{}", app.hypervisor_id);
        let (status, _) = app.status_of(Method::DELETE, &hypervisor).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "an instance is deployed on it"
        );

        app.status_of(Method::DELETE, &instance_path).await;
        let (status, _) = app.status_of(Method::DELETE, &path).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn the_whole_lab_counts_in_the_quota() {
        let app = TestApp::new().await;
        let template = built_template(&app, true).await;
        let (lab, _) = router_lab(&app, &template).await;
        let (user, token) = app.user(&unique("carol"), "user").await;
        let lab_id = lab.rsplit('/').next().unwrap();
        app.send(
            Method::PUT,
            &format!("{}/{lab_id}/permissions/users/{user}", labs(&app)),
            Some(json!({})),
        )
        .await;
        app.send(
            Method::PUT,
            &format!("/api/users/{user}/quota"),
            Some(json!({ "max_vms": 2 })),
        )
        .await;
        let (status, json) = deploy(&app, &token, &lab, false).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "3 VMs for a quota of 2: {json}"
        );
        assert!(json["content"].as_str().unwrap().contains("max_vms"));
    }

    #[tokio::test]
    async fn a_failed_deployment_leaves_nothing_behind() {
        let app = TestApp::new().await;
        // Built in the database, but its disk is missing: the VM cannot be created.
        let template = built_template(&app, false).await;
        let (lab, _) = router_lab(&app, &template).await;
        let (status, instance) = deploy(&app, &app.admin_token, &lab, false).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let path = format!("{lab}/instances/{}", instance["id"].as_str().unwrap());
        let failed = wait_instance(&app, &app.admin_token, &path).await;
        assert_eq!(failed["status"], "failed");
        assert!(
            failed["error"].as_str().unwrap().contains("disk"),
            "{failed}"
        );
        assert!(failed["networks"].as_array().unwrap().is_empty());
        assert!(failed["vms"].as_array().unwrap().is_empty());
        let short = &instance["id"].as_str().unwrap().replace('-', "")[..8];
        assert!(!network_exists(&format!("lab-{short}-lan1")));

        // A failed instance is deleted like the others.
        let (status, _) = app.status_of(Method::DELETE, &path).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
}
