//! VM templates built with Packer, their versions, and VMs created from them.
//!
//! A template is an editable definition (Packer configuration, disk name, pool). Each
//! build of it makes a new numbered version with its own disk, state and log, so a template
//! can be rebuilt (after an edit, or to pick up updates) while VMs keep being created from
//! its last good version. A VM gets its own full copy of the version's disk.
//!
//! Rights:
//! * Admins create and delete templates, choose their visibility and give rights on them.
//! * A right (`.../permissions`) goes to a user or to a group, and so to every user in it,
//!   nested groups included. `use`: see the built versions and create VMs from them.
//!   `manage`: also edit, rebuild and stop the template, see every build with its
//!   configuration and its log, and delete versions. A Packer configuration runs arbitrary
//!   commands (provisioners, `shell-local`) on the machine that builds it: giving `manage`
//!   is trusting with that machine.
//! * A right may limit how many VMs from the template each user covered owns (`max_vms`).
//!   When several rights apply (the user's, their groups'), the most generous one wins.
//! * A `hypervisor` template can also be used, without limit, by every user granted its
//!   hypervisor; a `restricted` one only through rights. A right is enough to create VMs
//!   from the template, even without a grant on the hypervisor.
//! * Users see a template only once it has a built version (managers see it at once),
//!   and never its configuration nor its logs. They own the VMs they create from it, within
//!   their quota, as with `POST .../vms`.
//! * VM creation can be turned off for a template (`allow_vm_creation`), for everyone.
//!
//! Builds run in the background, one at a time per template and `MAX_CONCURRENT_BUILDS`
//! overall. A version goes `pending` (waiting for a build slot), `in_progress`, then
//! `created`, `failed` or `cancelled` (`stopping` while a stop is in effect). Its log is
//! kept with it. A build cut by a restart of the API is reported as failed.
//!
//! Packer runs on the hypervisor over SSH for a `qemu+ssh://` URI (same user, port and key
//! as libvirt), on the API host for a local URI such as `qemu:///system`. That machine
//! needs `packer`, `qemu-img` and `virsh`. A stop sends SIGINT to the build there (Packer
//! cancels and cleans up), a forced stop SIGKILL; both reach its whole process group.

use std::{
    collections::HashMap,
    process::Stdio,
    sync::{Arc, LazyLock, Mutex, MutexGuard},
    time::Duration,
};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{StatusCode, header},
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::{Semaphore, watch},
    time::{Instant, MissedTickBehavior},
};
use url::Url;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;
use virt::{
    connect::Connect, domain::Domain, error::ErrorNumber, network::Network,
    storage_pool::StoragePool, storage_vol::StorageVol, sys,
};

use crate::{
    access::{self, Usage, VmAccess},
    audit,
    auth::{self, AuthUser, now},
    errors::ApiError,
    groups,
    jobs::Job,
    libvirt,
    models::Message,
    state::SharedState,
    vm_create, vm_options, vm_xml,
};

const MAX_PACKER_CONFIG_LEN: usize = 64 * 1024;
/// A build installs a whole OS: the next ones wait as `pending`.
const MAX_CONCURRENT_BUILDS: usize = 2;
/// Default of `PACKER_TIMEOUT_SECONDS`.
const DEFAULT_BUILD_TIMEOUT: Duration = Duration::from_secs(2 * 3600);
/// After a stop, how long Packer has to clean up before the build is killed.
const STOP_GRACE: Duration = Duration::from_secs(120);
/// How often the log of a running build is saved.
const LOG_FLUSH_INTERVAL: Duration = Duration::from_secs(5);
/// A process left behind by a killed build may keep the output open: stop reading then.
const PIPE_DRAIN: Duration = Duration::from_secs(5);
const SIGNAL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_LOG_LEN: usize = 2 * 1024 * 1024;
/// How much of the output is kept as the error of a failed build.
const MAX_ERROR_LEN: usize = 4000;
const LOG_TRUNCATED: &str = "[log truncated: the build output exceeded the limit]";
const DEFAULT_POOL: &str = "default";
const GIB: u64 = 1024 * 1024 * 1024;

const TEMPLATE_COLUMNS: &str = "t.id, t.hypervisor_id, t.name, t.packer_config, t.disk_name, t.pool, t.visibility, t.allow_vm_creation, t.created_at, t.updated_at";
const VERSION_COLUMNS: &str = "template_id, version, packer_config, disk_name, pool, volume, status, disk_size_gb, error, created_at, started_at, finished_at";
const BUILDING: &str = "('pending', 'in_progress', 'stopping')";

static BUILD_SLOTS: Semaphore = Semaphore::const_new(MAX_CONCURRENT_BUILDS);
/// Builds whose task is alive in this process, by `build_key`.
static RUNNING: LazyLock<Mutex<HashMap<String, Arc<BuildControl>>>> =
    LazyLock::new(Default::default);
/// Unfinished versions created before this time belong to a previous run of the API.
static STARTED_AT: LazyLock<i64> = LazyLock::new(now);
/// VMs being created from a template, by (template, owner): they count in its limit.
static PENDING_VMS: LazyLock<Mutex<HashMap<(String, String), i64>>> =
    LazyLock::new(Default::default);

// ---------- models ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum TemplateStatus {
    /// Waiting for a build slot.
    Pending,
    /// Packer is building the disk.
    InProgress,
    /// A stop was asked: Packer is cleaning up.
    Stopping,
    /// The disk is in its pool: VMs can be created from it.
    Created,
    Failed,
    /// Stopped by an administrator.
    Cancelled,
}

impl TemplateStatus {
    fn is_building(self) -> bool {
        matches!(
            self,
            TemplateStatus::Pending | TemplateStatus::InProgress | TemplateStatus::Stopping
        )
    }
}

/// Who may use a template besides admins and the users and groups given a right on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum Visibility {
    /// Also every user granted the hypervisor, without limit (default).
    Hypervisor,
    /// Only through rights (`.../permissions`).
    Restricted,
}

/// A right on a template.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema, sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum TemplateRole {
    /// See the built versions and create VMs from them.
    Use,
    /// Also edit, rebuild and stop the template, read its logs and delete versions.
    Manage,
}

/// What the caller may do with a template.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessLevel {
    Use,
    Manage,
    Admin,
}

/// The caller's rights on a template, all rights that apply to them combined.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub struct TemplateAccess {
    pub level: AccessLevel,
    /// How many VMs from this template the caller may own; null: no limit.
    pub max_vms: Option<i64>,
    /// VMs the caller owns that were created from this template.
    pub vm_count: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    User,
    Group,
}

/// A right given to a user or a group.
#[derive(Serialize, ToSchema)]
pub struct TemplatePermission {
    pub kind: PrincipalKind,
    /// Id of the user or of the group.
    pub id: String,
    /// Username, or group name.
    pub name: String,
    pub role: TemplateRole,
    /// VMs from this template each user covered may own; null: no limit.
    pub max_vms: Option<i64>,
    /// For a user: the VMs they own from this template.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vm_count: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Body of `PUT .../permissions/users/{user_id}` and `.../permissions/groups/{group_id}`.
#[derive(Deserialize, ToSchema)]
pub struct SetPermission {
    /// Default: `use`.
    pub role: Option<TemplateRole>,
    /// VMs from this template each user covered may own. Absent or null: no limit.
    pub max_vms: Option<i64>,
}

#[derive(FromRow)]
struct TemplateRow {
    id: String,
    hypervisor_id: String,
    name: String,
    packer_config: String,
    disk_name: String,
    pool: String,
    visibility: Visibility,
    allow_vm_creation: bool,
    created_at: i64,
    updated_at: i64,
}

#[derive(Serialize, ToSchema)]
pub struct Template {
    pub id: String,
    pub hypervisor_id: String,
    pub name: String,
    /// Managers and admins only. Edited with PATCH, used by the next build.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packer_config: Option<String>,
    /// File name of the disk image Packer writes.
    pub disk_name: String,
    /// Pool receiving the built disks.
    pub pool: String,
    pub visibility: Visibility,
    /// When false, nobody (admins included) can create a VM from this template.
    pub allow_vm_creation: bool,
    /// What the caller may do with it.
    pub access: TemplateAccess,
    /// State of the latest version (for users: of the latest built one).
    pub status: Option<TemplateStatus>,
    /// Version used to create VMs when none is given: the latest built one.
    pub default_version: Option<i64>,
    pub latest_version: Option<TemplateVersion>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One build of a template.
#[derive(Serialize, ToSchema, FromRow)]
pub struct TemplateVersion {
    pub template_id: String,
    pub version: i64,
    /// Managers and admins only: the configuration this version was built from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packer_config: Option<String>,
    pub disk_name: String,
    pub pool: String,
    /// libvirt volume holding the built disk.
    pub volume: String,
    pub status: TemplateStatus,
    /// Virtual size of the built disk, once created.
    pub disk_size_gb: Option<i64>,
    /// Managers and admins only: why the build failed (end of its output).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateTemplate {
    pub name: String,
    /// Packer configuration (HCL2, or its JSON form), run on the build host.
    pub packer_config: String,
    /// File name of the disk image the configuration produces (`vm_name` of the qemu builder).
    pub disk_name: String,
    /// Pool receiving the built disks (default: `default`).
    pub pool: Option<String>,
    /// Default: `hypervisor`.
    pub visibility: Option<Visibility>,
    /// Allow creating VMs from this template (default: true).
    pub allow_vm_creation: Option<bool>,
}

/// Fields left out keep their value. Existing versions are not changed: rebuild to apply.
/// `visibility` is for admins only.
#[derive(Deserialize, ToSchema)]
pub struct UpdateTemplate {
    pub name: Option<String>,
    pub packer_config: Option<String>,
    pub disk_name: Option<String>,
    pub pool: Option<String>,
    pub visibility: Option<Visibility>,
    pub allow_vm_creation: Option<bool>,
}

/// Body of `POST .../templates/{template_id}/vms`. The disk is a copy of the version's.
#[derive(Deserialize, ToSchema)]
pub struct CreateVmFromTemplate {
    pub name: String,
    pub memory_mb: u64,
    pub vcpus: u64,
    /// Built version to use (default: the latest built one).
    pub version: Option<i64>,
    /// Virtual network to plug the VM into (default: `default`).
    pub network: Option<String>,
    /// `bios` (default) or `uefi`: must match how the template was installed.
    pub firmware: Option<String>,
    /// Start the VM right after its creation (default: false).
    pub start: Option<bool>,
    /// Admin only: id of the user who will own the VM (default: the caller).
    pub owner_id: Option<String>,
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LogQuery {
    /// Byte offset to read from: the `next_offset` of the previous call (default: 0).
    offset: Option<usize>,
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StopQuery {
    /// Kill the build at once instead of letting Packer clean up (default: false).
    force: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct BuildLog {
    pub template_id: String,
    pub version: i64,
    pub status: TemplateStatus,
    /// True once the build ended: the log will not grow any more.
    pub finished: bool,
    pub offset: usize,
    /// Pass it as `offset` to get only what was written since.
    pub next_offset: usize,
    pub content: String,
    /// The output exceeded the limit: its end was not kept (see `error`).
    pub truncated: bool,
}

type Accepted<T> = (StatusCode, [(header::HeaderName, String); 1], Json<T>);

fn accepted<T>(location: String, body: T) -> Accepted<T> {
    (
        StatusCode::ACCEPTED,
        [(header::LOCATION, location)],
        Json(body),
    )
}

fn template_path(hypervisor_id: &str, template_id: &str) -> String {
    format!("/api/hypervisors/{hypervisor_id}/templates/{template_id}")
}

// ---------- templates ----------

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/templates", params(("hypervisor_id" = String, Path)), responses((status = 200, body = [Template], description = "The templates the caller has a right on (admins: all)"), (status = 404, body = Message)), tag = "Templates")]
pub async fn list(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(hypervisor_id): Path<String>,
) -> Result<Json<Vec<Template>>, ApiError> {
    libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    fail_interrupted(&state).await?;
    let caller = Caller::of(&state, &user, &hypervisor_id).await?;
    let rows = sqlx::query_as::<_, TemplateRow>(&format!(
        "SELECT {TEMPLATE_COLUMNS} FROM templates t WHERE t.hypervisor_id = $1 ORDER BY t.name"
    ))
    .bind(&hypervisor_id)
    .fetch_all(&state.db)
    .await?;
    let mut templates = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(rights) = caller.rights(&state, &row).await? {
            templates.push(view(&state, &user, row, rights).await?);
        }
    }
    Ok(Json(templates))
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}", params(("hypervisor_id" = String, Path), ("template_id" = String, Path)), responses((status = 200, body = Template), (status = 404, body = Message)), tag = "Templates")]
pub async fn get(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id)): Path<(String, String)>,
) -> Result<Json<Template>, ApiError> {
    fail_interrupted(&state).await?;
    let (row, rights) = find_template(&state, &user, &hypervisor_id, &template_id).await?;
    Ok(Json(view(&state, &user, row, rights).await?))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/templates", params(("hypervisor_id" = String, Path)), request_body = CreateTemplate, responses((status = 202, body = Template, description = "Template created and version 1 being built: follow it with GET .../templates/{template_id}"), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message, description = "Hypervisor or pool not found"), (status = 409, body = Message)), tag = "Templates")]
pub async fn create(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(hypervisor_id): Path<String>,
    Json(body): Json<CreateTemplate>,
) -> Result<Accepted<Template>, ApiError> {
    // The configuration runs commands on the build host: admins only.
    user.require_admin()?;
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    let definition = Definition {
        name: body.name,
        packer_config: body.packer_config,
        disk_name: body.disk_name,
        pool: body.pool.unwrap_or_else(|| DEFAULT_POOL.into()),
    };
    definition.validate()?;
    BuildTarget::from_uri(&uri)?;
    {
        let (uri, pool) = (uri.clone(), definition.pool.clone());
        blocking(move || active_pool(&Connect::open(Some(&uri))?, &pool).map(|_| ())).await?;
    }

    let id = Uuid::new_v4().to_string();
    let created_at = now();
    sqlx::query(
        "INSERT INTO templates (id, hypervisor_id, name, packer_config, disk_name, pool, visibility, allow_vm_creation, created_by, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(&id)
    .bind(&hypervisor_id)
    .bind(&definition.name)
    .bind(&definition.packer_config)
    .bind(&definition.disk_name)
    .bind(&definition.pool)
    .bind(body.visibility.unwrap_or(Visibility::Hypervisor))
    .bind(body.allow_vm_creation.unwrap_or(true))
    .bind(&user.id)
    .bind(created_at)
    .bind(created_at)
    .execute(&state.db)
    .await
    .map_err(name_conflict)?;
    if let Err(error) = start_build(&state, &user, &hypervisor_id, &uri, &id).await {
        sqlx::query("DELETE FROM templates WHERE id = $1")
            .bind(&id)
            .execute(&state.db)
            .await?;
        return Err(error);
    }
    audit::record(
        &state,
        &user,
        "template.create",
        &format!("{hypervisor_id}/{id}"),
        &definition.name,
    )
    .await;

    let (row, rights) = find_template(&state, &user, &hypervisor_id, &id).await?;
    let template = view(&state, &user, row, rights).await?;
    Ok(accepted(template_path(&hypervisor_id, &id), template))
}

#[utoipa::path(patch, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}", params(("hypervisor_id" = String, Path), ("template_id" = String, Path)), request_body = UpdateTemplate, responses((status = 200, body = Template), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message), (status = 409, body = Message)), tag = "Templates")]
pub async fn update(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id)): Path<(String, String)>,
    Json(body): Json<UpdateTemplate>,
) -> Result<Json<Template>, ApiError> {
    libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    let (row, rights) = find_template(&state, &user, &hypervisor_id, &template_id).await?;
    rights.require_manage()?;
    // Who may use the template is decided by admins.
    if body.visibility.is_some() {
        user.require_admin()?;
    }
    let definition = Definition {
        name: body.name.unwrap_or(row.name),
        packer_config: body.packer_config.unwrap_or(row.packer_config),
        disk_name: body.disk_name.unwrap_or(row.disk_name),
        pool: body.pool.unwrap_or(row.pool),
    };
    definition.validate()?;
    sqlx::query(
        "UPDATE templates SET name = $1, packer_config = $2, disk_name = $3, pool = $4, visibility = $5, allow_vm_creation = $6, updated_at = $7
         WHERE id = $8",
    )
    .bind(&definition.name)
    .bind(&definition.packer_config)
    .bind(&definition.disk_name)
    .bind(&definition.pool)
    .bind(body.visibility.unwrap_or(row.visibility))
    .bind(body.allow_vm_creation.unwrap_or(row.allow_vm_creation))
    .bind(now())
    .bind(&template_id)
    .execute(&state.db)
    .await
    .map_err(name_conflict)?;
    audit::record(
        &state,
        &user,
        "template.update",
        &format!("{hypervisor_id}/{template_id}"),
        &definition.name,
    )
    .await;
    let (row, rights) = find_template(&state, &user, &hypervisor_id, &template_id).await?;
    Ok(Json(view(&state, &user, row, rights).await?))
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}", params(("hypervisor_id" = String, Path), ("template_id" = String, Path)), responses((status = 204, description = "Template, versions and their disks deleted (VMs made from it keep their own copy)"), (status = 403, body = Message), (status = 404, body = Message), (status = 409, body = Message)), tag = "Templates")]
pub async fn delete(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    fail_interrupted(&state).await?;
    let (row, _) = find_template(&state, &user, &hypervisor_id, &template_id).await?;
    if building_version(&state, &row.id).await?.is_some() {
        return Err(ApiError::Conflict(
            "The template is being built: stop the build first".into(),
        ));
    }
    let disks: Vec<(String, String)> =
        sqlx::query_as("SELECT pool, volume FROM template_versions WHERE template_id = $1")
            .bind(&row.id)
            .fetch_all(&state.db)
            .await?;
    blocking(move || {
        for (pool, volume) in &disks {
            delete_volume(&uri, pool, volume)?;
        }
        Ok(())
    })
    .await?;
    sqlx::query("DELETE FROM templates WHERE id = $1")
        .bind(&row.id)
        .execute(&state.db)
        .await?;
    audit::record(
        &state,
        &user,
        "template.delete",
        &format!("{hypervisor_id}/{}", row.id),
        &row.name,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- versions ----------

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}/versions", params(("hypervisor_id" = String, Path), ("template_id" = String, Path)), responses((status = 200, body = [TemplateVersion], description = "Newest first. Users of the template only get the built versions"), (status = 404, body = Message)), tag = "Templates")]
pub async fn list_versions(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id)): Path<(String, String)>,
) -> Result<Json<Vec<TemplateVersion>>, ApiError> {
    fail_interrupted(&state).await?;
    let (row, rights) = find_template(&state, &user, &hypervisor_id, &template_id).await?;
    let versions = sqlx::query_as::<_, TemplateVersion>(&format!(
        "SELECT {VERSION_COLUMNS} FROM template_versions WHERE template_id = $1 AND ($2 OR status = 'created')
         ORDER BY version DESC"
    ))
    .bind(&row.id)
    .bind(rights.manages())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        versions
            .into_iter()
            .map(|version| present_version(rights, version))
            .collect(),
    ))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}/versions", params(("hypervisor_id" = String, Path), ("template_id" = String, Path)), responses((status = 202, body = TemplateVersion, description = "New version being built from the current definition"), (status = 403, body = Message), (status = 404, body = Message), (status = 409, body = Message, description = "A build of this template is already running")), tag = "Templates")]
pub async fn rebuild(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id)): Path<(String, String)>,
) -> Result<Accepted<TemplateVersion>, ApiError> {
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    fail_interrupted(&state).await?;
    let (row, rights) = find_template(&state, &user, &hypervisor_id, &template_id).await?;
    rights.require_manage()?;
    let version = start_build(&state, &user, &hypervisor_id, &uri, &row.id).await?;
    audit::record(
        &state,
        &user,
        "template.rebuild",
        &format!("{hypervisor_id}/{}", row.id),
        &format!("version {}", version.version),
    )
    .await;
    let location = format!(
        "{}/versions/{}",
        template_path(&hypervisor_id, &row.id),
        version.version
    );
    Ok(accepted(location, version))
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}/versions/{version}", params(("hypervisor_id" = String, Path), ("template_id" = String, Path), ("version" = i64, Path)), responses((status = 200, body = TemplateVersion), (status = 404, body = Message)), tag = "Templates")]
pub async fn get_version(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id, version)): Path<(String, String, i64)>,
) -> Result<Json<TemplateVersion>, ApiError> {
    fail_interrupted(&state).await?;
    let (row, rights) = find_template(&state, &user, &hypervisor_id, &template_id).await?;
    let version = find_version(&state, rights.manages(), &row.id, version).await?;
    Ok(Json(present_version(rights, version)))
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}/versions/{version}", params(("hypervisor_id" = String, Path), ("template_id" = String, Path), ("version" = i64, Path)), responses((status = 204, description = "Version and its disk deleted"), (status = 403, body = Message), (status = 404, body = Message), (status = 409, body = Message)), tag = "Templates")]
pub async fn delete_version(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id, version)): Path<(String, String, i64)>,
) -> Result<StatusCode, ApiError> {
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    fail_interrupted(&state).await?;
    let (row, rights) = find_template(&state, &user, &hypervisor_id, &template_id).await?;
    rights.require_manage()?;
    let version = find_version(&state, true, &row.id, version).await?;
    if version.status.is_building() {
        return Err(ApiError::Conflict(
            "This version is being built: stop the build first".into(),
        ));
    }
    let (pool, volume) = (version.pool.clone(), version.volume.clone());
    blocking(move || delete_volume(&uri, &pool, &volume)).await?;
    sqlx::query("DELETE FROM template_versions WHERE template_id = $1 AND version = $2")
        .bind(&row.id)
        .bind(version.version)
        .execute(&state.db)
        .await?;
    audit::record(
        &state,
        &user,
        "template.version_delete",
        &format!("{hypervisor_id}/{}", row.id),
        &format!("version {}", version.version),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}/versions/{version}/logs", params(("hypervisor_id" = String, Path), ("template_id" = String, Path), ("version" = i64, Path), LogQuery), responses((status = 200, body = BuildLog, description = "Live while the build runs: poll with offset = next_offset"), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message)), tag = "Templates")]
pub async fn logs(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id, version)): Path<(String, String, i64)>,
    Query(query): Query<LogQuery>,
) -> Result<Json<BuildLog>, ApiError> {
    libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    fail_interrupted(&state).await?;
    let (row, rights) = find_template(&state, &user, &hypervisor_id, &template_id).await?;
    // Build output shows the configuration at work, secrets included.
    rights.require_manage()?;
    let version = find_version(&state, true, &row.id, version).await?;
    // Read after the state: a build that ended meanwhile has saved its whole log.
    let text = match control_of(&build_key(&row.id, version.version)) {
        Some(control) => control.log().text.clone(),
        None => {
            sqlx::query_scalar(
                "SELECT log FROM template_versions WHERE template_id = $1 AND version = $2",
            )
            .bind(&row.id)
            .bind(version.version)
            .fetch_one(&state.db)
            .await?
        }
    };
    let offset = query.offset.unwrap_or(0);
    if offset > text.len() || !text.is_char_boundary(offset) {
        return Err(bad(
            "Invalid offset: use the next_offset of a previous call",
        ));
    }
    Ok(Json(BuildLog {
        template_id: row.id,
        version: version.version,
        status: version.status,
        finished: !version.status.is_building(),
        offset,
        next_offset: text.len(),
        truncated: text.contains(LOG_TRUNCATED),
        content: text[offset..].to_owned(),
    }))
}

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}/stop", params(("hypervisor_id" = String, Path), ("template_id" = String, Path), StopQuery), responses((status = 202, body = TemplateVersion, description = "Stop under way: the version ends as cancelled"), (status = 403, body = Message), (status = 404, body = Message), (status = 409, body = Message, description = "No build is running")), tag = "Templates")]
pub async fn stop(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id)): Path<(String, String)>,
    Query(query): Query<StopQuery>,
) -> Result<Accepted<TemplateVersion>, ApiError> {
    libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    fail_interrupted(&state).await?;
    let (row, rights) = find_template(&state, &user, &hypervisor_id, &template_id).await?;
    rights.require_manage()?;
    let no_build = || ApiError::Conflict("No build of this template is running".into());
    let number = building_version(&state, &row.id)
        .await?
        .ok_or_else(no_build)?;
    let control = control_of(&build_key(&row.id, number)).ok_or_else(no_build)?;
    let force = query.force.unwrap_or(false);
    let wanted = if force {
        StopMode::Force
    } else {
        StopMode::Graceful
    };
    // A forced stop overrides a graceful one, never the reverse.
    control.stop.send_if_modified(|mode| {
        if *mode == Some(StopMode::Force) || *mode == Some(wanted) {
            return false;
        }
        *mode = Some(wanted);
        true
    });
    sqlx::query(
        "UPDATE template_versions SET status = 'stopping'
         WHERE template_id = $1 AND version = $2 AND status IN ('pending', 'in_progress')",
    )
    .bind(&row.id)
    .bind(number)
    .execute(&state.db)
    .await?;
    let action = if force {
        "template.stop_force"
    } else {
        "template.stop"
    };
    audit::record(
        &state,
        &user,
        action,
        &format!("{hypervisor_id}/{}", row.id),
        &format!("version {number}"),
    )
    .await;
    let version = find_version(&state, true, &row.id, number).await?;
    let location = format!(
        "{}/versions/{number}",
        template_path(&hypervisor_id, &row.id)
    );
    Ok(accepted(location, version))
}

// ---------- permissions ----------

#[utoipa::path(get, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}/permissions", params(("hypervisor_id" = String, Path), ("template_id" = String, Path)), responses((status = 200, body = [TemplatePermission], description = "Users first, then groups"), (status = 403, body = Message), (status = 404, body = Message)), tag = "Templates")]
pub async fn list_permissions(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id)): Path<(String, String)>,
) -> Result<Json<Vec<TemplatePermission>>, ApiError> {
    user.require_admin()?;
    let (row, _) = find_template(&state, &user, &hypervisor_id, &template_id).await?;
    let entries = sqlx::query_as::<_, PermissionRow>(
        "SELECT 'user' AS kind, p.user_id AS id, u.username AS name, p.role, p.max_vms, p.created_at, p.updated_at
         FROM template_permissions p JOIN users u ON u.id = p.user_id WHERE p.template_id = $1
         UNION ALL
         SELECT 'group', p.group_id, g.name, p.role, p.max_vms, p.created_at, p.updated_at
         FROM template_permissions p JOIN user_groups g ON g.id = p.group_id WHERE p.template_id = $1
         ORDER BY 1 DESC, 3",
    )
    .bind(&row.id)
    .fetch_all(&state.db)
    .await?;
    let mut permissions = Vec::with_capacity(entries.len());
    for entry in entries {
        let (kind, vm_count) = if entry.kind == "user" {
            (
                PrincipalKind::User,
                Some(vms_from(&state, &row.id, &entry.id).await?),
            )
        } else {
            (PrincipalKind::Group, None)
        };
        permissions.push(TemplatePermission {
            kind,
            id: entry.id,
            name: entry.name,
            role: entry.role,
            max_vms: entry.max_vms,
            vm_count,
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
    max_vms: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

#[utoipa::path(put, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}/permissions/users/{user_id}", params(("hypervisor_id" = String, Path), ("template_id" = String, Path), ("user_id" = String, Path)), request_body = SetPermission, responses((status = 204, description = "Right given or changed"), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message)), tag = "Templates")]
pub async fn set_user_permission(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id, user_id)): Path<(String, String, String)>,
    Json(body): Json<SetPermission>,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    let (row, _) = find_template(&state, &user, &hypervisor_id, &template_id).await?;
    let name: Option<String> = sqlx::query_scalar("SELECT username FROM users WHERE id = $1")
        .bind(&user_id)
        .fetch_optional(&state.db)
        .await?;
    let name = name.ok_or_else(|| ApiError::NotFound("User not found".into()))?;
    set_permission(&state, &row.id, Principal::User, &user_id, &body).await?;
    audit::record(
        &state,
        &user,
        "template.permission",
        &format!("{hypervisor_id}/{}", row.id),
        &permission_detail("user", &name, &body),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(put, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}/permissions/groups/{group_id}", params(("hypervisor_id" = String, Path), ("template_id" = String, Path), ("group_id" = String, Path)), request_body = SetPermission, responses((status = 204, description = "Right given or changed, for every user of the group and of its nested groups"), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message)), tag = "Templates")]
pub async fn set_group_permission(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id, group_id)): Path<(String, String, String)>,
    Json(body): Json<SetPermission>,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    let (row, _) = find_template(&state, &user, &hypervisor_id, &template_id).await?;
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM user_groups WHERE id = $1")
        .bind(&group_id)
        .fetch_optional(&state.db)
        .await?;
    let name = name.ok_or_else(|| ApiError::NotFound("Group not found".into()))?;
    set_permission(&state, &row.id, Principal::Group, &group_id, &body).await?;
    audit::record(
        &state,
        &user,
        "template.permission",
        &format!("{hypervisor_id}/{}", row.id),
        &permission_detail("group", &name, &body),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}/permissions/users/{user_id}", params(("hypervisor_id" = String, Path), ("template_id" = String, Path), ("user_id" = String, Path)), responses((status = 204, description = "Right removed: the VMs already created are kept"), (status = 403, body = Message), (status = 404, body = Message)), tag = "Templates")]
pub async fn remove_user_permission(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id, user_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    remove_permission(
        &state,
        &user,
        &hypervisor_id,
        &template_id,
        Principal::User,
        &user_id,
    )
    .await
}

#[utoipa::path(delete, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}/permissions/groups/{group_id}", params(("hypervisor_id" = String, Path), ("template_id" = String, Path), ("group_id" = String, Path)), responses((status = 204, description = "Right removed: the VMs already created are kept"), (status = 403, body = Message), (status = 404, body = Message)), tag = "Templates")]
pub async fn remove_group_permission(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id, group_id)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    remove_permission(
        &state,
        &user,
        &hypervisor_id,
        &template_id,
        Principal::Group,
        &group_id,
    )
    .await
}

/// Who a right is given to.
#[derive(Clone, Copy)]
enum Principal {
    User,
    Group,
}

impl Principal {
    fn column(self) -> &'static str {
        match self {
            Principal::User => "user_id",
            Principal::Group => "group_id",
        }
    }
}

async fn set_permission(
    state: &SharedState,
    template_id: &str,
    principal: Principal,
    principal_id: &str,
    body: &SetPermission,
) -> Result<(), ApiError> {
    if body.max_vms.is_some_and(|max| max < 0) {
        return Err(bad("max_vms must be 0 or more (null: no limit)"));
    }
    let column = principal.column();
    let at = now();
    sqlx::query(&format!(
        "INSERT INTO template_permissions (template_id, {column}, role, max_vms, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (template_id, {column}) WHERE {column} IS NOT NULL
         DO UPDATE SET role = excluded.role, max_vms = excluded.max_vms, updated_at = excluded.updated_at"
    ))
    .bind(template_id)
    .bind(principal_id)
    .bind(body.role.unwrap_or(TemplateRole::Use))
    .bind(body.max_vms)
    .bind(at)
    .bind(at)
    .execute(&state.db)
    .await?;
    Ok(())
}

async fn remove_permission(
    state: &SharedState,
    user: &AuthUser,
    hypervisor_id: &str,
    template_id: &str,
    principal: Principal,
    principal_id: &str,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    let (row, _) = find_template(state, user, hypervisor_id, template_id).await?;
    let column = principal.column();
    let result = sqlx::query(&format!(
        "DELETE FROM template_permissions WHERE template_id = $1 AND {column} = $2"
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
        "template.permission_remove",
        &format!("{hypervisor_id}/{}", row.id),
        &format!("{column} {principal_id}"),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

fn permission_detail(kind: &str, name: &str, body: &SetPermission) -> String {
    let role = body.role.unwrap_or(TemplateRole::Use);
    match body.max_vms {
        Some(max) => format!("{kind} {name}: {role:?}, at most {max} VMs"),
        None => format!("{kind} {name}: {role:?}, no limit"),
    }
}

// ---------- VMs ----------

#[utoipa::path(post, path = "/api/hypervisors/{hypervisor_id}/templates/{template_id}/vms", params(("hypervisor_id" = String, Path), ("template_id" = String, Path)), request_body = CreateVmFromTemplate, responses((status = 202, body = Job, description = "Creation started (the disk is copied): follow it with GET /api/jobs/{job_id}"), (status = 400, body = Message), (status = 403, body = Message, description = "Quota or template limit (max_vms) reached"), (status = 404, body = Message), (status = 409, body = Message)), tag = "Templates")]
pub async fn create_vm(
    State(state): State<SharedState>,
    user: AuthUser,
    Path((hypervisor_id, template_id)): Path<(String, String)>,
    Json(body): Json<CreateVmFromTemplate>,
) -> Result<Accepted<Job>, ApiError> {
    let uri = libvirt::hypervisor_uri(&state.db, &hypervisor_id).await?;
    // Any right on the template allows it (`use` or `manage`): no hypervisor grant needed.
    let (row, rights) = find_template(&state, &user, &hypervisor_id, &template_id).await?;
    if !row.allow_vm_creation {
        return Err(ApiError::Conflict(
            "Creating VMs from this template is disabled".into(),
        ));
    }
    let version = match body.version {
        Some(number) => find_version(&state, rights.manages(), &row.id, number).await?,
        None => {
            let latest: Option<i64> = sqlx::query_scalar(
                "SELECT MAX(version) FROM template_versions WHERE template_id = $1 AND status = 'created'",
            )
            .bind(&row.id)
            .fetch_one(&state.db)
            .await?;
            let latest = latest.ok_or_else(|| {
                ApiError::Conflict("The template has no built version yet".into())
            })?;
            find_version(&state, rights.manages(), &row.id, latest).await?
        }
    };
    if version.status != TemplateStatus::Created {
        return Err(ApiError::Conflict(
            "This version of the template is not built".into(),
        ));
    }
    let owner_id = owner_of_new_vm(&state, &user, body.owner_id.as_deref()).await?;
    validate_vm(&body)?;

    let disk_gb = version.disk_size_gb.unwrap_or(0).max(0) as u64;
    let usage = Usage::one_vm(body.vcpus, body.memory_mb, disk_gb);
    let new_vm = NewVm {
        uri: uri.clone(),
        name: body.name.clone(),
        memory_mb: body.memory_mb,
        vcpus: body.vcpus,
        uefi: body.firmware.as_deref() == Some("uefi"),
        start: body.start.unwrap_or(false),
        network: body
            .network
            .clone()
            .unwrap_or_else(|| vm_options::DEFAULT_NETWORK.into()),
        pool: version.pool.clone(),
        template_volume: version.volume.clone(),
        vnc_listen: vm_create::vnc_listen()?,
    };
    // Cheap checks first, so obvious mistakes fail at once instead of inside the job.
    let new_vm = blocking(move || check_new_vm(&new_vm).map(|_| new_vm)).await?;

    // Under the lock, so that two requests cannot both pass the check of the last free slot.
    let (job, pending) = {
        let _quota = state.quota_lock.lock().await;
        access::check_quota(&state, &owner_id, usage).await?;
        // Admins are not limited (an admin creating a VM for a user neither).
        if let (false, Some(max_vms)) = (user.is_admin(), rights.max_vms) {
            let owned =
                vms_from(&state, &row.id, &owner_id).await? + PendingVm::count(&row.id, &owner_id);
            if owned >= max_vms {
                return Err(ApiError::Forbidden(format!(
                    "Template limit reached: you may own at most {max_vms} VMs from this template"
                )));
            }
        }
        let pending = PendingVm::new(&row.id, &owner_id);
        (state.jobs.start("template_vm", &owner_id, usage), pending)
    };
    audit::record(
        &state,
        &user,
        "template.vm_create",
        &format!("{hypervisor_id}/{}", row.id),
        &format!("{} (version {})", body.name, version.version),
    )
    .await;

    let job_id = job.id.clone();
    let task_state = Arc::clone(&state);
    let (template_id, version_number) = (row.id.clone(), version.version);
    tokio::spawn(async move {
        let outcome = match blocking(move || define_from_template(&new_vm)).await {
            Ok(vm_id) => libvirt::get_vm(uri, hypervisor_id.clone(), vm_id).await,
            Err(error) => Err(error),
        };
        // The owner is saved before the job is declared finished: no window where the
        // client sees a VM it cannot access.
        let outcome = match outcome {
            Ok(vm) => {
                let recorded =
                    access::record_owner(&task_state, &hypervisor_id, &vm.id, &owner_id, usage)
                        .await;
                let recorded = match recorded {
                    Ok(()) => {
                        record_origin(
                            &task_state,
                            &hypervisor_id,
                            &vm.id,
                            &template_id,
                            version_number,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                };
                match recorded {
                    Ok(()) => {
                        let vm_access = if owner_id == user.id {
                            VmAccess::Owner
                        } else {
                            VmAccess::Admin
                        };
                        access::describe_vm(&task_state, &user, vm_access, vm).await
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        };
        // The VM now counts through template_vms: no longer as pending.
        drop(pending);
        task_state.jobs.finish(&job_id, outcome);
    });

    Ok(accepted(format!("/api/jobs/{}", job.id), job))
}

// ---------- lookups and views ----------

async fn find_template(
    state: &SharedState,
    user: &AuthUser,
    hypervisor_id: &str,
    template_id: &str,
) -> Result<(TemplateRow, Rights), ApiError> {
    let not_found = || ApiError::NotFound("Template not found".into());
    let row = sqlx::query_as::<_, TemplateRow>(&format!(
        "SELECT {TEMPLATE_COLUMNS} FROM templates t WHERE t.hypervisor_id = $1 AND t.id = $2"
    ))
    .bind(hypervisor_id)
    .bind(template_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(not_found)?;
    // Someone with no right on the template gets a 404, as if it did not exist.
    let rights = Caller::of(state, user, hypervisor_id)
        .await?
        .rights(state, &row)
        .await?
        .ok_or_else(not_found)?;
    Ok((row, rights))
}

/// `all`: every build (managers); otherwise only the built versions.
async fn find_version(
    state: &SharedState,
    all: bool,
    template_id: &str,
    version: i64,
) -> Result<TemplateVersion, ApiError> {
    sqlx::query_as::<_, TemplateVersion>(&format!(
        "SELECT {VERSION_COLUMNS} FROM template_versions
         WHERE template_id = $1 AND version = $2 AND ($3 OR status = 'created')"
    ))
    .bind(template_id)
    .bind(version)
    .bind(all)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::NotFound("Template version not found".into()))
}

async fn building_version(state: &SharedState, template_id: &str) -> Result<Option<i64>, ApiError> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT version FROM template_versions WHERE template_id = $1 AND status IN {BUILDING}"
    ))
    .bind(template_id)
    .fetch_optional(&state.db)
    .await?)
}

async fn view(
    state: &SharedState,
    user: &AuthUser,
    row: TemplateRow,
    rights: Rights,
) -> Result<Template, ApiError> {
    let latest = sqlx::query_as::<_, TemplateVersion>(&format!(
        "SELECT {VERSION_COLUMNS} FROM template_versions WHERE template_id = $1 AND ($2 OR status = 'created')
         ORDER BY version DESC LIMIT 1"
    ))
    .bind(&row.id)
    .bind(rights.manages())
    .fetch_optional(&state.db)
    .await?;
    let default_version: Option<i64> = sqlx::query_scalar(
        "SELECT MAX(version) FROM template_versions WHERE template_id = $1 AND status = 'created'",
    )
    .bind(&row.id)
    .fetch_one(&state.db)
    .await?;
    let access = TemplateAccess {
        level: rights.level,
        max_vms: rights.max_vms,
        vm_count: vms_from(state, &row.id, &user.id).await?,
    };
    Ok(Template {
        status: latest.as_ref().map(|version| version.status),
        latest_version: latest.map(|version| present_version(rights, version)),
        default_version,
        // The configuration can hold secrets: only those managing the template get it.
        packer_config: rights.manages().then_some(row.packer_config),
        access,
        id: row.id,
        hypervisor_id: row.hypervisor_id,
        name: row.name,
        disk_name: row.disk_name,
        pool: row.pool,
        visibility: row.visibility,
        allow_vm_creation: row.allow_vm_creation,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// The configuration and the build output can hold secrets: only managers get them.
fn present_version(rights: Rights, mut version: TemplateVersion) -> TemplateVersion {
    if !rights.manages() {
        version.packer_config = None;
        version.error = None;
    }
    version
}

// ---------- rights ----------

/// What a caller may do with one template: every right that applies to them, combined.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rights {
    level: AccessLevel,
    /// None: no limit.
    max_vms: Option<i64>,
}

impl Rights {
    /// Adds a right: the highest level and the most generous limit win.
    fn with(self, level: AccessLevel, max_vms: Option<i64>) -> Rights {
        Rights {
            level: self.level.max(level),
            max_vms: match (self.max_vms, max_vms) {
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
            Err(ApiError::Forbidden(
                "You do not manage this template".into(),
            ))
        }
    }
}

impl TemplateRole {
    fn level(self) -> AccessLevel {
        match self {
            TemplateRole::Use => AccessLevel::Use,
            TemplateRole::Manage => AccessLevel::Manage,
        }
    }
}

/// One right on a template: given to a user, or to a group.
#[derive(FromRow)]
struct RightRow {
    user_id: Option<String>,
    group_id: Option<String>,
    role: TemplateRole,
    max_vms: Option<i64>,
}

/// The caller, with what decides their rights on the templates of one hypervisor.
struct Caller<'a> {
    user: &'a AuthUser,
    /// The groups they are in, nested ones included.
    groups: Vec<String>,
    /// Granted the hypervisor: its `hypervisor` templates are open to them.
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

    /// None when the caller may not even see the template.
    async fn rights(
        &self,
        state: &SharedState,
        row: &TemplateRow,
    ) -> Result<Option<Rights>, ApiError> {
        if self.user.is_admin() {
            return Ok(Some(Rights {
                level: AccessLevel::Admin,
                max_vms: None,
            }));
        }
        let entries = sqlx::query_as::<_, RightRow>(
            "SELECT user_id, group_id, role, max_vms FROM template_permissions WHERE template_id = $1",
        )
        .bind(&row.id)
        .fetch_all(&state.db)
        .await?;
        let mut rights: Option<Rights> = None;
        let mut add = |level: AccessLevel, max_vms: Option<i64>| {
            rights = Some(match rights {
                Some(rights) => rights.with(level, max_vms),
                None => Rights { level, max_vms },
            });
        };
        for entry in entries {
            let applies = entry.user_id.as_deref() == Some(self.user.id.as_str())
                || entry
                    .group_id
                    .is_some_and(|group| self.groups.contains(&group));
            if applies {
                add(entry.role.level(), entry.max_vms);
            }
        }
        if row.visibility == Visibility::Hypervisor && self.granted {
            add(AccessLevel::Use, None);
        }
        // Users see a template once it can be used; those managing it, at once.
        if let Some(found) = rights
            && !found.manages()
        {
            let built: Option<i64> = sqlx::query_scalar(
                "SELECT 1::BIGINT FROM template_versions WHERE template_id = $1 AND status = 'created' LIMIT 1",
            )
            .bind(&row.id)
            .fetch_optional(&state.db)
            .await?;
            if built.is_none() {
                return Ok(None);
            }
        }
        Ok(rights)
    }
}

/// VMs owned by `owner_id` that were created from the template.
async fn vms_from(state: &SharedState, template_id: &str, owner_id: &str) -> Result<i64, ApiError> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM template_vms tv
         JOIN vm_owners o ON o.hypervisor_id = tv.hypervisor_id AND o.vm_id = tv.vm_id
         WHERE tv.template_id = $1 AND o.owner_id = $2",
    )
    .bind(template_id)
    .bind(owner_id)
    .fetch_one(&state.db)
    .await?)
}

/// Remembers which template (and version) a VM was created from.
async fn record_origin(
    state: &SharedState,
    hypervisor_id: &str,
    vm_id: &str,
    template_id: &str,
    version: i64,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO template_vms (hypervisor_id, vm_id, template_id, version, created_at)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (hypervisor_id, vm_id) DO UPDATE
         SET template_id = excluded.template_id, version = excluded.version, created_at = excluded.created_at",
    )
    .bind(hypervisor_id)
    .bind(access::parse_vm_id(vm_id)?)
    .bind(template_id)
    .bind(version)
    .bind(now())
    .execute(&state.db)
    .await?;
    Ok(())
}

/// A VM being created from a template: counted in its limit until the job ends, even if
/// the task panics.
struct PendingVm {
    key: (String, String),
}

impl PendingVm {
    fn new(template_id: &str, owner_id: &str) -> PendingVm {
        let key = (template_id.to_owned(), owner_id.to_owned());
        *pending_vms().entry(key.clone()).or_default() += 1;
        PendingVm { key }
    }

    fn count(template_id: &str, owner_id: &str) -> i64 {
        pending_vms()
            .get(&(template_id.to_owned(), owner_id.to_owned()))
            .copied()
            .unwrap_or(0)
    }
}

impl Drop for PendingVm {
    fn drop(&mut self) {
        let mut pending = pending_vms();
        if let Some(count) = pending.get_mut(&self.key) {
            *count -= 1;
            if *count <= 0 {
                pending.remove(&self.key);
            }
        }
    }
}

// Never held across an await: a poisoned mutex is still consistent.
fn pending_vms() -> MutexGuard<'static, HashMap<(String, String), i64>> {
    PENDING_VMS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Only an admin can create a VM for someone else.
async fn owner_of_new_vm(
    state: &SharedState,
    user: &AuthUser,
    owner_id: Option<&str>,
) -> Result<String, ApiError> {
    let Some(owner_id) = owner_id else {
        return Ok(user.id.clone());
    };
    user.require_admin()?;
    let exists: Option<i64> =
        sqlx::query_scalar("SELECT 1::BIGINT FROM users WHERE id = $1 AND NOT disabled")
            .bind(owner_id)
            .fetch_optional(&state.db)
            .await?;
    exists.ok_or_else(|| ApiError::NotFound("Owner not found".into()))?;
    Ok(owner_id.to_owned())
}

fn name_conflict(error: sqlx::Error) -> ApiError {
    match error {
        sqlx::Error::Database(e) if e.is_unique_violation() => {
            ApiError::Conflict("A template with this name already exists on this hypervisor".into())
        }
        other => other.into(),
    }
}

// ---------- validation ----------

/// What a build is made from.
struct Definition {
    name: String,
    packer_config: String,
    disk_name: String,
    pool: String,
}

impl Definition {
    fn validate(&self) -> Result<(), ApiError> {
        // Both end up in file names and in the build script.
        vm_xml::validate_name(&self.name).map_err(|_| {
            bad("name must be 1-64 characters: letters, digits, '-', '_' or '.' (not leading)")
        })?;
        vm_xml::validate_name(&self.disk_name).map_err(|_| {
            bad("disk_name must be the file name of the disk Packer writes: 1-64 characters, letters, digits, '-', '_' or '.' (not leading)")
        })?;
        label("pool", &self.pool)?;
        let config = &self.packer_config;
        if config.trim().is_empty() || config.len() > MAX_PACKER_CONFIG_LEN || config.contains('\0')
        {
            return Err(bad(&format!(
                "packer_config must be a non-empty text of at most {MAX_PACKER_CONFIG_LEN} bytes"
            )));
        }
        Ok(())
    }
}

fn validate_vm(request: &CreateVmFromTemplate) -> Result<(), ApiError> {
    vm_xml::validate_name(&request.name)?;
    in_range(
        "memory_mb",
        request.memory_mb,
        vm_options::MIN_MEMORY_MB,
        vm_options::MAX_MEMORY_MB,
    )?;
    in_range(
        "vcpus",
        request.vcpus,
        vm_options::MIN_VCPUS,
        vm_options::MAX_VCPUS,
    )?;
    if let Some(network) = &request.network {
        label("network", network)?;
    }
    if let Some(firmware) = &request.firmware
        && !vm_options::FIRMWARES.contains(&firmware.as_str())
    {
        return Err(bad(&format!(
            "firmware must be one of {:?}",
            vm_options::FIRMWARES
        )));
    }
    Ok(())
}

fn bad(message: &str) -> ApiError {
    ApiError::BadRequest(message.into())
}

fn in_range(field: &str, value: u64, min: u64, max: u64) -> Result<(), ApiError> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(bad(&format!("{field} must be between {min} and {max}")))
    }
}

/// Names looked up in libvirt: printable, no path separator (a NUL byte would also make
/// the C call panic).
fn label(field: &str, value: &str) -> Result<(), ApiError> {
    if value.is_empty()
        || value.chars().count() > 255
        || value.chars().any(|c| c.is_control() || c == '/')
    {
        Err(bad(&format!("Invalid {field}")))
    } else {
        Ok(())
    }
}

// ---------- build ----------

#[derive(Debug, PartialEq)]
enum BuildHost {
    /// The API host itself (local libvirt URI).
    Local,
    Ssh {
        destination: String,
        port: Option<u16>,
        keyfile: Option<String>,
    },
}

/// Where Packer runs, derived from the libvirt URI of the hypervisor.
#[derive(Debug, PartialEq)]
struct BuildTarget {
    host: BuildHost,
    /// The hypervisor's libvirt URI as seen from the build host, for `virsh`.
    libvirt_uri: String,
}

impl BuildTarget {
    fn from_uri(uri: &str) -> Result<Self, ApiError> {
        let url = Url::parse(uri).map_err(|_| bad("Invalid hypervisor URI"))?;
        let (driver, transport) = url.scheme().split_once('+').unwrap_or((url.scheme(), ""));
        match (transport, url.host_str().filter(|host| !host.is_empty())) {
            ("ssh", Some(host)) => {
                let destination = match url.username() {
                    "" => host.to_owned(),
                    user => format!("{user}@{host}"),
                };
                let keyfile = url
                    .query_pairs()
                    .find(|(key, _)| key == "keyfile")
                    .map(|(_, value)| value.into_owned());
                Ok(Self {
                    host: BuildHost::Ssh {
                        destination,
                        port: url.port(),
                        keyfile,
                    },
                    libvirt_uri: format!("{driver}://{}", url.path()),
                })
            }
            (_, None) => Ok(Self {
                host: BuildHost::Local,
                libvirt_uri: uri.to_owned(),
            }),
            _ => Err(bad(
                "Templates need a local or qemu+ssh:// hypervisor: Packer is run over SSH",
            )),
        }
    }

    /// Runs a shell command on the build host.
    fn command(&self, shell_command: &str) -> Command {
        match &self.host {
            BuildHost::Local => {
                let mut command = Command::new("sh");
                command.arg("-c").arg(shell_command);
                command
            }
            BuildHost::Ssh {
                destination,
                port,
                keyfile,
            } => {
                let mut command = Command::new("ssh");
                command.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15"]);
                if let Some(port) = port {
                    command.arg("-p").arg(port.to_string());
                }
                if let Some(keyfile) = keyfile {
                    command.arg("-i").arg(keyfile);
                }
                // `--`: the destination can never be read as an option.
                command.arg("--").arg(destination).arg(shell_command);
                command
            }
        }
    }
}

/// Creates the next version from the current definition and starts building it. Only for
/// callers managing the template: the whole version is returned, configuration included.
async fn start_build(
    state: &SharedState,
    user: &AuthUser,
    hypervisor_id: &str,
    uri: &str,
    template_id: &str,
) -> Result<TemplateVersion, ApiError> {
    LazyLock::force(&STARTED_AT);
    let target = BuildTarget::from_uri(uri)?;
    // Numbered and snapshotted in one statement; the partial unique index refuses a second
    // build of the same template.
    let inserted: Option<(i64, String)> = sqlx::query_as(
        "INSERT INTO template_versions (template_id, version, packer_config, disk_name, pool, volume, status, created_by, created_at)
         SELECT t.id, n.v, t.packer_config, t.disk_name, t.pool, 'template-' || t.name || '-v' || n.v || '.qcow2', 'pending', $2, $3
         FROM templates t, (SELECT COALESCE(MAX(version), 0) + 1 AS v FROM template_versions WHERE template_id = $1) n
         WHERE t.id = $1
         RETURNING version, volume",
    )
    .bind(template_id)
    .bind(&user.id)
    .bind(now())
    .fetch_optional(&state.db)
    .await
    .map_err(|error| match error {
        sqlx::Error::Database(e) if e.is_unique_violation() => {
            ApiError::Conflict("A build of this template is already running".into())
        }
        other => other.into(),
    })?;
    let (number, volume) =
        inserted.ok_or_else(|| ApiError::NotFound("Template not found".into()))?;
    let version = find_version(state, true, template_id, number).await?;

    let prepared = async {
        let (uri, pool, volume) = (uri.to_owned(), version.pool.clone(), volume.clone());
        blocking(move || check_destination(&uri, &pool, &volume)).await?;
        build_script(
            version.packer_config.as_deref().unwrap_or_default(),
            &version.disk_name,
            &version.pool,
            &version.volume,
            &target.libvirt_uri,
        )
    }
    .await;
    let script = match prepared {
        Ok(script) => script,
        Err(error) => {
            sqlx::query("DELETE FROM template_versions WHERE template_id = $1 AND version = $2")
                .bind(template_id)
                .bind(number)
                .execute(&state.db)
                .await?;
            return Err(error);
        }
    };

    let key = build_key(template_id, number);
    let control = Arc::new(BuildControl::default());
    running().insert(key.clone(), Arc::clone(&control));
    tokio::spawn(build(
        Arc::clone(state),
        Build {
            key,
            template_id: template_id.to_owned(),
            version: number,
            hypervisor_id: hypervisor_id.to_owned(),
            uri: uri.to_owned(),
            target,
            script,
            pool: version.pool.clone(),
            volume,
        },
        control,
    ));
    Ok(version)
}

fn build_key(template_id: &str, version: i64) -> String {
    format!("{template_id}/{version}")
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum StopMode {
    /// SIGINT: Packer cancels the build and cleans up.
    Graceful,
    /// SIGKILL to everything, and the work directory removed.
    Force,
}

/// Shared between a running build and the requests that follow or stop it.
struct BuildControl {
    log: Mutex<LogBuffer>,
    stop: watch::Sender<Option<StopMode>>,
    process: Mutex<BuildProcess>,
}

impl Default for BuildControl {
    fn default() -> Self {
        Self {
            log: Default::default(),
            stop: watch::Sender::new(None),
            process: Default::default(),
        }
    }
}

// These locks are never held across an await: a poisoned mutex is still consistent.
impl BuildControl {
    fn log(&self) -> MutexGuard<'_, LogBuffer> {
        self.log.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn process(&self) -> MutexGuard<'_, BuildProcess> {
        self.process.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Takes `@@build ...` lines, which the script prints first; true if it was one.
    fn announce(&self, line: &str) -> bool {
        if let Some(group) = line.strip_prefix("@@build pgid=") {
            // 0, 1 or a negative number would signal far more than the build.
            if let Ok(group) = group.trim().parse::<i32>()
                && group > 1
            {
                self.process().group = Some(group);
            }
            return true;
        }
        if let Some(dir) = line.strip_prefix("@@build work=") {
            if dir.starts_with('/')
                && dir.contains("/hypervisor-template.")
                && !dir.contains("..")
                && !dir.chars().any(char::is_control)
            {
                self.process().work_dir = Some(dir.to_owned());
            }
            return true;
        }
        false
    }
}

/// The process of a build on its host, as the script announced it.
#[derive(Clone, Default)]
struct BuildProcess {
    group: Option<i32>,
    work_dir: Option<String>,
}

#[derive(Default)]
struct LogBuffer {
    text: String,
    /// The last lines, kept even when `text` is full: the error of a failed build.
    recent: String,
    truncated: bool,
}

impl LogBuffer {
    fn push(&mut self, line: &str) {
        self.recent.push_str(line);
        self.recent.push('\n');
        if self.recent.len() > 2 * MAX_ERROR_LEN * 4 {
            let mut cut = self.recent.len() - MAX_ERROR_LEN * 4;
            while !self.recent.is_char_boundary(cut) {
                cut += 1;
            }
            self.recent.drain(..cut);
        }
        if self.truncated {
            return;
        }
        if self.text.len() + line.len() + 1 > MAX_LOG_LEN {
            self.text.push_str(LOG_TRUNCATED);
            self.text.push('\n');
            self.truncated = true;
            return;
        }
        self.text.push_str(line);
        self.text.push('\n');
    }
}

fn running() -> MutexGuard<'static, HashMap<String, Arc<BuildControl>>> {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner())
}

fn control_of(key: &str) -> Option<Arc<BuildControl>> {
    running().get(key).cloned()
}

/// Everything the background build needs.
struct Build {
    key: String,
    template_id: String,
    version: i64,
    hypervisor_id: String,
    uri: String,
    target: BuildTarget,
    script: String,
    pool: String,
    volume: String,
}

enum Outcome {
    /// Size of the disk, in GiB.
    Created(i64),
    Failed(String),
    Cancelled,
}

async fn build(state: SharedState, build: Build, control: Arc<BuildControl>) {
    let outcome = run_build(&state, &build, &control).await;
    if !matches!(outcome, Outcome::Created(_)) {
        // A volume with this name did not exist before the build (`check_destination`):
        // whatever is there now is a leftover of this build.
        let (uri, pool, volume) = (build.uri.clone(), build.pool.clone(), build.volume.clone());
        if let Err(error) = blocking(move || delete_volume(&uri, &pool, &volume)).await {
            eprintln!(
                "Could not remove the disk of an unfinished build: {}",
                error.message()
            );
        }
    }
    let log = control.log().text.clone();
    let (status, size_gb, error) = match &outcome {
        Outcome::Created(size_gb) => (TemplateStatus::Created, Some(*size_gb), None),
        Outcome::Failed(error) => {
            eprintln!(
                "Build of template {} version {} failed",
                build.template_id, build.version
            );
            (TemplateStatus::Failed, None, Some(error.clone()))
        }
        Outcome::Cancelled => (
            TemplateStatus::Cancelled,
            None,
            Some("Stopped by an administrator".to_owned()),
        ),
    };
    let saved = sqlx::query(
        "UPDATE template_versions SET status = $1, disk_size_gb = $2, error = $3, log = $4, finished_at = $5
         WHERE template_id = $6 AND version = $7",
    )
    .bind(status)
    .bind(size_gb)
    .bind(error)
    .bind(log)
    .bind(now())
    .bind(&build.template_id)
    .bind(build.version)
    .execute(&state.db)
    .await;
    if let Err(error) = saved {
        eprintln!("Could not save the end of a template build: {error}");
    }
    audit::insert(
        &state,
        None,
        None,
        "template.build",
        &format!("{}/{}", build.hypervisor_id, build.template_id),
        &format!("version {}: {status:?}", build.version),
        status == TemplateStatus::Created,
    )
    .await;
    // Only once the final state is saved, so that readers never miss it.
    running().remove(&build.key);
}

async fn run_build(state: &SharedState, build: &Build, control: &Arc<BuildControl>) -> Outcome {
    let mut stop = control.stop.subscribe();
    let _slot = tokio::select! {
        slot = BUILD_SLOTS.acquire() => match slot {
            Ok(slot) => slot,
            Err(_) => return Outcome::Failed("The build queue is closed".into()),
        },
        _ = stop.wait_for(Option::is_some) => return Outcome::Cancelled,
    };
    if stop.borrow().is_some() {
        return Outcome::Cancelled;
    }
    let started = sqlx::query(
        "UPDATE template_versions SET status = 'in_progress', started_at = $1
         WHERE template_id = $2 AND version = $3 AND status = 'pending'",
    )
    .bind(now())
    .bind(&build.template_id)
    .bind(build.version)
    .execute(&state.db)
    .await;
    if let Err(error) = started {
        return Outcome::Failed(format!("Could not start the build: {error}"));
    }
    if let Err(outcome) = run_script(state, build, control, &mut stop).await {
        return outcome;
    }
    let (uri, pool, volume) = (build.uri.clone(), build.pool.clone(), build.volume.clone());
    match blocking(move || register_volume(&uri, &pool, &volume)).await {
        Ok(size_gb) => Outcome::Created(size_gb),
        Err(error) => Outcome::Failed(error.message().to_owned()),
    }
}

/// Runs the script on the build host, following its output and the stop requests.
async fn run_script(
    state: &SharedState,
    build: &Build,
    control: &Arc<BuildControl>,
    stop: &mut watch::Receiver<Option<StopMode>>,
) -> Result<(), Outcome> {
    let mut child = build
        .target
        .command("bash -s")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        // Its own process group: the signals of a stop never reach the API.
        .process_group(0)
        .spawn()
        .map_err(|e| Outcome::Failed(format!("Could not start the build: {e}")))?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    if let Err(error) = stdin.write_all(build.script.as_bytes()).await {
        let _ = child.start_kill();
        return Err(Outcome::Failed(format!(
            "Could not send the build script: {error}"
        )));
    }
    drop(stdin);
    let stdout = tokio::spawn(pump(
        child.stdout.take().expect("stdout is piped"),
        Arc::clone(control),
        true,
    ));
    let stderr = tokio::spawn(pump(
        child.stderr.take().expect("stderr is piped"),
        Arc::clone(control),
        false,
    ));

    let timeout = build_timeout();
    // The build timeout, then the end of the grace period of a stop.
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut flush = tokio::time::interval(LOG_FLUSH_INTERVAL);
    flush.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut flushed = 0;
    let mut stopping: Option<StopMode> = None;
    let mut timed_out = false;
    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            _ = flush.tick() => flushed = flush_log(state, build, control, flushed).await,
            _ = &mut deadline, if stopping != Some(StopMode::Force) => {
                timed_out = stopping.is_none();
                kill_build(&build.target, control, &mut child).await;
                stopping = Some(StopMode::Force);
            }
            changed = stop.changed(), if stopping != Some(StopMode::Force) => {
                if changed.is_err() {
                    continue;
                }
                let wanted = *stop.borrow_and_update();
                match wanted {
                    Some(StopMode::Force) => {
                        kill_build(&build.target, control, &mut child).await;
                        stopping = Some(StopMode::Force);
                    }
                    Some(StopMode::Graceful) if stopping.is_none() => {
                        signal_build(&build.target, control, "INT").await;
                        deadline.as_mut().reset(Instant::now() + STOP_GRACE);
                        stopping = Some(StopMode::Graceful);
                    }
                    _ => {}
                }
            }
        }
    };
    let _ = tokio::time::timeout(PIPE_DRAIN, stdout).await;
    let _ = tokio::time::timeout(PIPE_DRAIN, stderr).await;

    if timed_out {
        return Err(Outcome::Failed(format!(
            "The build timed out after {} seconds (PACKER_TIMEOUT_SECONDS)",
            timeout.as_secs()
        )));
    }
    match status {
        Ok(status) if status.success() => Ok(()),
        // A build that finished as it was being stopped is kept.
        _ if stopping.is_some() => Err(Outcome::Cancelled),
        Ok(_) => Err(Outcome::Failed(tail(&control.log().recent, MAX_ERROR_LEN))),
        Err(error) => Err(Outcome::Failed(format!(
            "The build could not be followed: {error}"
        ))),
    }
}

/// Adds the lines of the build output to its log.
async fn pump<R: AsyncRead + Unpin>(reader: R, control: Arc<BuildControl>, announces: bool) {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    // Only the first lines of the script announce its process: later ones come from Packer
    // and the configuration, which must not choose what a stop kills.
    let mut announcements = if announces { 2 } else { 0 };
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim_end_matches(['\n', '\r']);
        if announcements > 0 {
            announcements -= 1;
            if control.announce(text) {
                continue;
            }
            announcements = 0;
        }
        control.log().push(text);
    }
}

/// Saves the log of a running build, so that a crash of the API does not lose it.
async fn flush_log(
    state: &SharedState,
    build: &Build,
    control: &BuildControl,
    flushed: usize,
) -> usize {
    let text = {
        let log = control.log();
        if log.text.len() == flushed {
            return flushed;
        }
        log.text.clone()
    };
    let saved = sqlx::query(
        "UPDATE template_versions SET log = $1 WHERE template_id = $2 AND version = $3",
    )
    .bind(&text)
    .bind(&build.template_id)
    .bind(build.version)
    .execute(&state.db)
    .await;
    match saved {
        Ok(_) => text.len(),
        Err(error) => {
            eprintln!("Could not save the log of a template build: {error}");
            flushed
        }
    }
}

/// Sends a signal to the whole process group of the build, on its host. A SIGKILL leaves
/// no chance to clean up: the work directory is removed too.
async fn signal_build(target: &BuildTarget, control: &BuildControl, signal: &str) {
    let process = control.process().clone();
    let Some(group) = process.group else {
        return;
    };
    let mut script = format!("kill -{signal} -- -{group}");
    if signal == "KILL"
        && let Some(dir) = &process.work_dir
    {
        script.push_str(&format!("; rm -rf -- {}", sh_quote(dir)));
    }
    let mut command = target.command(&script);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    match tokio::time::timeout(SIGNAL_TIMEOUT, command.status()).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => eprintln!("Could not signal a template build: {error}"),
        Err(_) => eprintln!("Signalling a template build timed out"),
    }
}

async fn kill_build(target: &BuildTarget, control: &BuildControl, child: &mut Child) {
    signal_build(target, control, "KILL").await;
    // The local end as well (ssh, or the shell of a local build).
    let _ = child.start_kill();
}

fn build_timeout() -> Duration {
    std::env::var("PACKER_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_BUILD_TIMEOUT)
}

/// Announces the process group and the work directory first (see `pump`), works in a
/// temporary directory (on disk, images are big) that is removed at the end.
const PREPARE: &str = r#"echo "@@build pgid=$(cut -d' ' -f5 /proc/$$/stat)"
work=$(mktemp -d "${TMPDIR:-/var/tmp}/hypervisor-template.XXXXXX")
echo "@@build work=$work"
trap 'rm -rf "$work"' EXIT
cd "$work"
"#;

const BUILD_AND_UPLOAD: &str = r#"echo "==> packer init"
packer init .
echo "==> packer build"
packer build -color=false .
disk=$(find "$work" -type f -name "$disk_name" ! -name "$config_file" -print -quit)
if [ -z "$disk" ]; then
    echo "Packer did not produce a file named $disk_name" >&2
    exit 1
fi
if [ "$(qemu-img info "$disk" | sed -n 's/^file format: //p')" != qcow2 ]; then
    qemu-img convert -O qcow2 "$disk" "$work/converted.qcow2"
    disk="$work/converted.qcow2"
fi
echo "==> upload to $pool/$volume"
virsh -q -c "$libvirt_uri" vol-create-as "$pool" "$volume" "$(stat -c %s "$disk")" --format qcow2
if ! virsh -q -c "$libvirt_uri" vol-upload --pool "$pool" "$volume" "$disk"; then
    virsh -q -c "$libvirt_uri" vol-delete --pool "$pool" "$volume" || true
    exit 1
fi
"#;

/// The shell script sent to the build host. Every value is quoted, and the configuration
/// is passed verbatim in a quoted here-document, so nothing in it is interpreted.
fn build_script(
    config: &str,
    disk_name: &str,
    pool: &str,
    volume: &str,
    libvirt_uri: &str,
) -> Result<String, ApiError> {
    let delimiter = format!("PACKER_CONFIG_{}", auth::random_token());
    if config.contains(&delimiter) {
        return Err(bad("Invalid packer_config"));
    }
    let config_file = if config.trim_start().starts_with('{') {
        "template.pkr.json"
    } else {
        "template.pkr.hcl"
    };
    // One `{ ... }` block: bash reads all of it before running anything, so no command
    // (packer...) can swallow the rest of the script from the standard input.
    let mut script = String::from("{\nset -eu\nexec </dev/null\n");
    for (name, value) in [
        ("libvirt_uri", libvirt_uri),
        ("pool", pool),
        ("volume", volume),
        ("disk_name", disk_name),
        ("config_file", config_file),
    ] {
        script.push_str(&format!("{name}={}\n", sh_quote(value)));
    }
    script.push_str(PREPARE);
    script.push_str(&format!(
        "cat > \"$config_file\" <<'{delimiter}'\n{config}\n{delimiter}\n"
    ));
    script.push_str(BUILD_AND_UPLOAD);
    script.push_str("}\n");
    Ok(script)
}

fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// The end of `text`, where the error of a failed build is.
fn tail(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    format!(
        "…{}",
        text.chars().skip(count - max_chars).collect::<String>()
    )
}

/// A build that is unfinished but was created before this process started belonged to a
/// previous run of the API: it was interrupted and never ends.
async fn fail_interrupted(state: &SharedState) -> Result<(), ApiError> {
    sqlx::query(&format!(
        "UPDATE template_versions SET status = 'failed', error = 'The build was interrupted by a restart of the API', finished_at = $1
         WHERE status IN {BUILDING} AND created_at < $2"
    ))
    .bind(now())
    .bind(*STARTED_AT)
    .execute(&state.db)
    .await?;
    Ok(())
}

// ---------- libvirt (blocking) ----------

/// libvirt is blocking: run it on a dedicated thread so the async runtime is not stalled.
async fn blocking<T, F>(job: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|e| ApiError::Internal(format!("Blocking task failed: {e}")))?
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

/// Whether the pool holds a volume of that name.
fn volume_exists(pool: &StoragePool, name: &str) -> Result<bool, ApiError> {
    match StorageVol::lookup_by_name(pool, name) {
        Ok(_) => Ok(true),
        Err(e) if e.code() == ErrorNumber::NoStorageVolume => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// The built disk can be stored: the pool is active and the volume name is free.
fn check_destination(uri: &str, pool_name: &str, volume: &str) -> Result<(), ApiError> {
    let conn = Connect::open(Some(uri))?;
    let pool = active_pool(&conn, pool_name)?;
    if volume_exists(&pool, volume)? {
        return Err(ApiError::Conflict(format!(
            "A volume named {volume} already exists in pool {pool_name}"
        )));
    }
    Ok(())
}

/// Makes libvirt see the uploaded volume and returns its virtual size in GiB.
fn register_volume(uri: &str, pool: &str, volume: &str) -> Result<i64, ApiError> {
    let conn = Connect::open(Some(uri))?;
    let pool = StoragePool::lookup_by_name(&conn, pool)?;
    pool.refresh(0)?;
    let volume = StorageVol::lookup_by_name(&pool, volume)?;
    Ok(volume.get_info()?.capacity.div_ceil(GIB) as i64)
}

fn delete_volume(uri: &str, pool: &str, volume: &str) -> Result<(), ApiError> {
    let conn = Connect::open(Some(uri))?;
    let volume = match StoragePool::lookup_by_name(&conn, pool)
        .and_then(|pool| StorageVol::lookup_by_name(&pool, volume))
    {
        Ok(volume) => volume,
        // Never built, or already removed: nothing to delete.
        Err(e)
            if matches!(
                e.code(),
                ErrorNumber::NoStoragePool | ErrorNumber::NoStorageVolume
            ) =>
        {
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    volume.delete(0).map_err(|e| match e.code() {
        ErrorNumber::OperationInvalid => ApiError::Conflict(
            "The disk of the template is in use (a VM is being created from it)".into(),
        ),
        _ => e.into(),
    })
}

/// A VM to create from a template version, checked by `check_new_vm`.
struct NewVm {
    uri: String,
    name: String,
    memory_mb: u64,
    vcpus: u64,
    uefi: bool,
    start: bool,
    network: String,
    pool: String,
    template_volume: String,
    vnc_listen: String,
}

impl NewVm {
    fn disk_volume(&self) -> String {
        format!("{}.qcow2", self.name)
    }
}

/// Checks everything the creation depends on, without changing anything.
fn check_new_vm(vm: &NewVm) -> Result<(), ApiError> {
    let conn = Connect::open(Some(&vm.uri))?;
    if Domain::lookup_by_name(&conn, &vm.name).is_ok() {
        return Err(ApiError::Conflict(
            "A VM with this name already exists".into(),
        ));
    }
    if !Network::lookup_by_name(&conn, &vm.network)?.is_active()? {
        return Err(ApiError::Conflict("The network is not active".into()));
    }
    let pool = active_pool(&conn, &vm.pool)?;
    if !volume_exists(&pool, &vm.template_volume)? {
        return Err(ApiError::Conflict(
            "The disk of the template is missing from its pool".into(),
        ));
    }
    if volume_exists(&pool, &vm.disk_volume())? {
        return Err(ApiError::Conflict(format!(
            "A volume named {} already exists in pool {}",
            vm.disk_volume(),
            vm.pool
        )));
    }
    Ok(())
}

/// Copies the template disk and defines the VM on it. Slow: the whole disk is copied.
/// Nothing is left behind if a step fails. Returns the UUID of the new VM.
fn define_from_template(vm: &NewVm) -> Result<String, ApiError> {
    let conn = Connect::open(Some(&vm.uri))?;
    let pool = active_pool(&conn, &vm.pool)?;
    let source = StorageVol::lookup_by_name(&pool, &vm.template_volume)?;
    let source_xml = source.get_xml_desc(0)?;
    let format = vm_xml::volume_format(&source_xml)?;
    let volume_xml = vm_xml::new_volume_xml(&source_xml, &vm.disk_volume())?;
    let disk = StorageVol::create_xml_from(&pool, &volume_xml, &source, 0)?;
    let result = define_on_disk(&conn, vm, &disk, &format);
    if result.is_err()
        && let Err(e) = disk.delete(0)
    {
        eprintln!("Could not remove the disk of a VM that failed to be created: {e}");
    }
    result
}

fn define_on_disk(
    conn: &Connect,
    vm: &NewVm,
    disk: &StorageVol,
    format: &str,
) -> Result<String, ApiError> {
    let spec = vm_xml::DomainSpec {
        name: vm.name.clone(),
        memory_mb: vm.memory_mb,
        vcpus: vm.vcpus,
        kvm: conn.get_type()? == "QEMU",
        uefi: vm.uefi,
        disks: vec![vm_xml::DiskAttachment {
            path: disk.get_path()?,
            format: format.to_owned(),
        }],
        iso_path: None,
        network: vm.network.clone(),
        vnc_listen: vm.vnc_listen.clone(),
    };
    let domain = Domain::define_xml(conn, &vm_xml::new_domain_xml(&spec)?)?;
    if vm.start
        && let Err(e) = domain.create()
    {
        eprintln!("Could not start a VM created from a template: {e}");
        if let Err(e) = domain.undefine_flags(sys::VIR_DOMAIN_UNDEFINE_NVRAM) {
            eprintln!("Could not remove a VM that failed to start: {e}");
        }
        return Err(ApiError::Internal(
            "The VM could not be started, it was not kept".into(),
        ));
    }
    Ok(domain.get_uuid_string()?)
}

#[cfg(test)]
mod tests {
    use axum::http::Method;
    use serde_json::{Value, json};

    use super::*;
    use crate::http_tests::{POOL, TestApp, connect, create_volume, unique};

    // ---------- pure functions ----------

    #[test]
    fn the_build_host_follows_the_libvirt_uri() {
        assert_eq!(
            BuildTarget::from_uri("qemu:///system").unwrap(),
            BuildTarget {
                host: BuildHost::Local,
                libvirt_uri: "qemu:///system".into()
            }
        );
        assert_eq!(
            BuildTarget::from_uri("qemu+ssh://admin@kvm1:2222/system?keyfile=/keys/id").unwrap(),
            BuildTarget {
                host: BuildHost::Ssh {
                    destination: "admin@kvm1".into(),
                    port: Some(2222),
                    keyfile: Some("/keys/id".into())
                },
                libvirt_uri: "qemu:///system".into()
            }
        );
        assert_eq!(
            BuildTarget::from_uri("qemu+ssh://kvm1/session").unwrap(),
            BuildTarget {
                host: BuildHost::Ssh {
                    destination: "kvm1".into(),
                    port: None,
                    keyfile: None
                },
                libvirt_uri: "qemu:///session".into()
            }
        );
        for unsupported in ["qemu+tcp://kvm1/system", "qemu://kvm1/system", "not a uri"] {
            assert!(
                matches!(
                    BuildTarget::from_uri(unsupported),
                    Err(ApiError::BadRequest(_))
                ),
                "{unsupported}"
            );
        }
    }

    #[test]
    fn the_script_keeps_the_config_verbatim_and_quotes_the_rest() {
        let config = "source \"qemu\" \"x\" {\n  boot_command = [\"$HOME `id` '\"]\n}";
        let script =
            build_script(config, "disk.qcow2", "it's", "v.qcow2", "qemu:///system").unwrap();
        assert!(script.contains(&format!("\n{config}\n")));
        assert!(script.contains(r"pool='it'\''s'"));
        assert!(script.contains("config_file='template.pkr.hcl'"));
        assert!(script.contains("<<'PACKER_CONFIG_"));
        let json = build_script("{\"builders\": []}", "d", "p", "v", "u").unwrap();
        assert!(json.contains("config_file='template.pkr.json'"));
    }

    #[tokio::test]
    async fn the_script_is_valid_shell() {
        let script = build_script(
            "a = \"$(id)\"\n",
            "disk.qcow2",
            "p'q",
            "v",
            "qemu:///system",
        )
        .unwrap();
        let mut child = Command::new("bash")
            .arg("-n")
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(script.as_bytes()).await.unwrap();
        drop(stdin);
        let output = child.wait_with_output().await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn a_long_output_keeps_its_end() {
        assert_eq!(tail("  short  ", 10), "short");
        assert_eq!(tail("0123456789", 3), "…789");
        assert_eq!(tail("ééééé", 2), "…éé");
    }

    #[test]
    fn only_sane_announcements_are_taken() {
        let control = BuildControl::default();
        assert!(control.announce("@@build pgid=1"));
        assert!(control.announce("@@build work=/etc"));
        assert!(control.announce("@@build work=/var/tmp/hypervisor-template.x/../.."));
        assert!(control.process().group.is_none() && control.process().work_dir.is_none());
        assert!(control.announce("@@build pgid=4242"));
        assert!(control.announce("@@build work=/var/tmp/hypervisor-template.AbC123"));
        assert_eq!(control.process().group, Some(4242));
        assert_eq!(
            control.process().work_dir.as_deref(),
            Some("/var/tmp/hypervisor-template.AbC123")
        );
        assert!(!control.announce("==> packer init"));
    }

    #[test]
    fn the_log_is_capped_but_keeps_its_end() {
        let mut log = LogBuffer::default();
        let line = "x".repeat(1000);
        for _ in 0..(MAX_LOG_LEN / 1000 + 10) {
            log.push(&line);
        }
        log.push("the real error");
        assert!(log.truncated);
        assert!(log.text.len() <= MAX_LOG_LEN + LOG_TRUNCATED.len() + 1);
        assert!(log.text.ends_with(&format!("{LOG_TRUNCATED}\n")));
        assert!(tail(&log.recent, MAX_ERROR_LEN).ends_with("the real error"));
    }

    // ---------- HTTP helpers ----------

    fn templates(app: &TestApp) -> String {
        format!("/api/hypervisors/{}/templates", app.hypervisor_id)
    }

    /// A user granted access to the test hypervisor.
    async fn granted_user(app: &TestApp) -> (String, String) {
        let (id, token) = app.user(&unique("user"), "user").await;
        let path = format!("/api/hypervisors/{}/access/{id}", app.hypervisor_id);
        let (status, _) = app.status_of(Method::PUT, &path).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        (id, token)
    }

    /// A time this process counts as its own (not a previous run of the API).
    fn live() -> i64 {
        LazyLock::force(&STARTED_AT);
        now()
    }

    /// A template whose version 1 is in a given state, as if built: returns its id and volume.
    async fn insert_template(app: &TestApp, status: &str) -> (String, String) {
        insert_template_at(app, status, live()).await
    }

    async fn insert_template_at(app: &TestApp, status: &str, created_at: i64) -> (String, String) {
        let name = unique("tpl");
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO templates (id, hypervisor_id, name, packer_config, disk_name, pool, created_at, updated_at)
             VALUES ($1, $2, $3, 'source \"null\" \"x\" {}', 'disk.qcow2', $4, 0, 0)",
        )
        .bind(&id)
        .bind(&app.hypervisor_id)
        .bind(&name)
        .bind(POOL)
        .execute(&app.state.db)
        .await
        .unwrap();
        let volume = insert_version(app, &id, &name, 1, status, created_at).await;
        (id, volume)
    }

    async fn insert_version(
        app: &TestApp,
        template_id: &str,
        name: &str,
        version: i64,
        status: &str,
        created_at: i64,
    ) -> String {
        let volume = format!("template-{name}-v{version}.qcow2");
        if status == "created" {
            create_volume(&volume);
        }
        sqlx::query(
            "INSERT INTO template_versions (template_id, version, packer_config, disk_name, pool, volume, status, disk_size_gb, created_at)
             VALUES ($1, $2, 'source \"null\" \"x\" {}', 'disk.qcow2', $3, $4, $5, 1, $6)",
        )
        .bind(template_id)
        .bind(version)
        .bind(POOL)
        .bind(&volume)
        .bind(status)
        .bind(created_at)
        .execute(&app.state.db)
        .await
        .unwrap();
        volume
    }

    fn template_body(name: &str, config: &str) -> Value {
        json!({ "name": name, "packer_config": config, "disk_name": "disk.qcow2", "pool": POOL })
    }

    const INVALID_CONFIG: &str = "this is not a valid packer configuration";

    /// Builds for a while and produces nothing: something to stop.
    const SLEEPING_CONFIG: &str = r#"source "null" "wait" { communicator = "none" }
build {
  sources = ["source.null.wait"]
  provisioner "shell-local" { inline = ["echo started", "sleep 300"] }
}"#;

    fn volume_in_pool(name: &str) -> bool {
        let pool = StoragePool::lookup_by_name(&connect(), POOL).unwrap();
        StorageVol::lookup_by_name(&pool, name).is_ok()
    }

    fn packer_installed() -> bool {
        std::process::Command::new("packer")
            .arg("version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    async fn create_template(app: &TestApp, name: &str, config: &str) -> Value {
        let (status, _, template) = app
            .send(
                Method::POST,
                &templates(app),
                Some(template_body(name, config)),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{template}");
        template
    }

    /// Polls a version until `done` says so.
    async fn wait_version(
        app: &TestApp,
        template_id: &str,
        version: i64,
        done: impl Fn(&Value) -> bool,
    ) -> Value {
        let path = format!("{}/{template_id}/versions/{version}", templates(app));
        let mut last = Value::Null;
        for _ in 0..600 {
            let (status, json) = app.status_of(Method::GET, &path).await;
            assert_eq!(status, StatusCode::OK, "{json}");
            if done(&json) {
                return json;
            }
            last = json;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("version {version} of {template_id} never got there: {last}");
    }

    fn ended(version: &Value) -> bool {
        matches!(
            version["status"].as_str(),
            Some("created" | "failed" | "cancelled")
        )
    }

    async fn logs(app: &TestApp, template_id: &str, version: i64, offset: usize) -> Value {
        let path = format!(
            "{}/{template_id}/versions/{version}/logs?offset={offset}",
            templates(app)
        );
        let (status, json) = app.status_of(Method::GET, &path).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        json
    }

    /// Waits until the sleeping build runs its provisioner, and returns its process.
    async fn wait_until_sleeping(app: &TestApp, template_id: &str) -> BuildProcess {
        for _ in 0..600 {
            let log = logs(app, template_id, 1, 0).await;
            if log["content"].as_str().unwrap().contains("started") {
                return control_of(&build_key(template_id, 1))
                    .unwrap()
                    .process()
                    .clone();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the build never started");
    }

    fn group_alive(group: i32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", "--", &format!("-{group}")])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    // ---------- rights ----------

    #[tokio::test]
    async fn users_cannot_manage_a_template_they_only_use() {
        let app = TestApp::new().await;
        let (user_id, token) = granted_user(&app).await;
        let (id, _) = insert_template(&app, "created").await;
        let template = format!("{}/{id}", templates(&app));
        let body = template_body(&unique("tpl"), INVALID_CONFIG);
        let (status, json) = app.status_as(&token, Method::GET, &template).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["access"]["level"], "use");
        for (method, path, body) in [
            (Method::POST, templates(&app), Some(body)),
            (
                Method::PATCH,
                template.clone(),
                Some(json!({ "name": "x" })),
            ),
            (Method::DELETE, template.clone(), None),
            (Method::POST, format!("{template}/versions"), None),
            (Method::DELETE, format!("{template}/versions/1"), None),
            (Method::GET, format!("{template}/versions/1/logs"), None),
            (Method::POST, format!("{template}/stop"), None),
            (Method::GET, format!("{template}/permissions"), None),
            (
                Method::PUT,
                format!("{template}/permissions/users/{user_id}"),
                Some(json!({ "role": "manage" })),
            ),
        ] {
            let (status, _, _) = app.send_as(Some(&token), method.clone(), &path, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
        }
        let (status, _, _) = app.send_as(None, Method::GET, &templates(&app), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn invalid_templates_are_rejected() {
        let app = TestApp::new().await;
        let mut cases = vec![];
        for (field, value) in [
            ("name", json!("../evil")),
            ("disk_name", json!("out/disk.qcow2")),
            ("packer_config", json!("   ")),
            (
                "packer_config",
                json!("x".repeat(MAX_PACKER_CONFIG_LEN + 1)),
            ),
            ("pool", json!("")),
            ("visibility", json!("everyone")),
        ] {
            let mut body = template_body(&unique("tpl"), INVALID_CONFIG);
            body[field] = value;
            cases.push((body, StatusCode::BAD_REQUEST));
        }
        let mut missing_pool = template_body(&unique("tpl"), INVALID_CONFIG);
        missing_pool["pool"] = json!("no-such-pool");
        cases.push((missing_pool, StatusCode::NOT_FOUND));

        for (body, expected) in cases {
            let (status, _, json) = app
                .send(Method::POST, &templates(&app), Some(body.clone()))
                .await;
            // An unknown enum value is refused by the JSON extractor (422).
            let expected = if body["visibility"] == "everyone" {
                StatusCode::UNPROCESSABLE_ENTITY
            } else {
                expected
            };
            assert_eq!(status, expected, "{body} -> {json}");
        }
        let unknown = format!("/api/hypervisors/{}/templates", Uuid::new_v4());
        let (status, _, _) = app
            .send(
                Method::POST,
                &unknown,
                Some(template_body(&unique("tpl"), INVALID_CONFIG)),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn users_see_only_built_versions_and_never_configurations_nor_logs() {
        let app = TestApp::new().await;
        let (built, built_volume) = insert_template(&app, "created").await;
        insert_template(&app, "failed").await;
        let name = built_volume
            .strip_prefix("template-")
            .unwrap()
            .strip_suffix("-v1.qcow2")
            .unwrap();
        insert_version(&app, &built, name, 2, "failed", live()).await;

        let (status, all) = app.status_of(Method::GET, &templates(&app)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(all.as_array().unwrap().len(), 2);
        assert!(all[0]["packer_config"].is_string());

        let (_, token) = granted_user(&app).await;
        let (status, mine) = app.status_as(&token, Method::GET, &templates(&app)).await;
        assert_eq!(status, StatusCode::OK);
        let mine = mine.as_array().unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0]["id"], built.as_str());
        assert_eq!(mine[0]["status"], "created");
        assert_eq!(mine[0]["default_version"], 1);
        assert_eq!(mine[0]["latest_version"]["version"], 1);
        assert!(mine[0].get("packer_config").is_none());

        let versions = format!("{}/{built}/versions", templates(&app));
        let (_, listed) = app.status_as(&token, Method::GET, &versions).await;
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert!(listed[0].get("packer_config").is_none());
        let (status, _) = app
            .status_as(&token, Method::GET, &format!("{versions}/2"))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (_, listed) = app.status_of(Method::GET, &versions).await;
        assert_eq!(
            listed.as_array().unwrap().len(),
            2,
            "admins see every build"
        );

        // No grant on the hypervisor and no right: no templates at all.
        let (_, stranger) = app.user(&unique("user"), "user").await;
        let (status, listed) = app
            .status_as(&stranger, Method::GET, &templates(&app))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(listed.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_restricted_template_is_only_for_those_given_a_right() {
        let app = TestApp::new().await;
        let (id, _) = insert_template(&app, "created").await;
        let template = format!("{}/{id}", templates(&app));
        let (status, _, json) = app
            .send(
                Method::PATCH,
                &template,
                Some(json!({ "visibility": "restricted" })),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["visibility"], "restricted");

        // A grant on the hypervisor is no longer enough.
        let (user_id, token) = granted_user(&app).await;
        let vm = json!({ "name": unique("vm"), "memory_mb": 128, "vcpus": 1 });
        let (_, listed) = app.status_as(&token, Method::GET, &templates(&app)).await;
        assert!(listed.as_array().unwrap().is_empty());
        let (status, _, _) = app
            .send_as(
                Some(&token),
                Method::POST,
                &format!("{template}/vms"),
                Some(vm),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let user_right = format!("{template}/permissions/users/{user_id}");
        let (status, _, _) = app.send(Method::PUT, &user_right, Some(json!({}))).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, entries) = app
            .status_of(Method::GET, &format!("{template}/permissions"))
            .await;
        assert_eq!(entries[0]["kind"], "user");
        assert_eq!(entries[0]["id"], user_id.as_str());
        assert_eq!(entries[0]["role"], "use");
        assert!(entries[0]["max_vms"].is_null());
        let (status, _) = app.status_as(&token, Method::GET, &template).await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = app.status_of(Method::DELETE, &user_right).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = app.status_as(&token, Method::GET, &template).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = app.status_of(Method::DELETE, &user_right).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    async fn group_with(app: &TestApp, user_id: &str) -> String {
        let (status, _, group) = app
            .send(
                Method::POST,
                "/api/groups",
                Some(json!({ "name": unique("group") })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
        let id = group["id"].as_str().unwrap().to_owned();
        let (status, _) = app
            .status_of(Method::PUT, &format!("/api/groups/{id}/members/{user_id}"))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        id
    }

    #[tokio::test]
    async fn a_right_given_to_a_group_reaches_its_nested_groups_without_a_hypervisor_grant() {
        let app = TestApp::new().await;
        let (id, _) = insert_template_at(&app, "created", live()).await;
        let template = format!("{}/{id}", templates(&app));
        app.send(
            Method::PATCH,
            &template,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;

        // company > team > dave; dave has no grant on the hypervisor.
        let (dave, token) = app.user(&unique("dave"), "user").await;
        let team = group_with(&app, &dave).await;
        let (_, _, company) = app
            .send(
                Method::POST,
                "/api/groups",
                Some(json!({ "name": unique("company") })),
            )
            .await;
        let company = company["id"].as_str().unwrap();
        app.status_of(
            Method::PUT,
            &format!("/api/groups/{company}/subgroups/{team}"),
        )
        .await;

        let right = format!("{template}/permissions/groups/{company}");
        let (status, _, _) = app.send(Method::PUT, &right, Some(json!({}))).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, listed) = app.status_as(&token, Method::GET, &templates(&app)).await;
        assert_eq!(listed.as_array().unwrap().len(), 1);
        let (status, _, job) = app
            .send_as(
                Some(&token),
                Method::POST,
                &format!("{template}/vms"),
                Some(json!({ "name": unique("vm"), "memory_mb": 128, "vcpus": 1 })),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{job}");
        assert_eq!(
            app.wait_job(job["id"].as_str().unwrap()).await["status"],
            "succeeded"
        );

        // Out of the nested group: no more right.
        app.status_of(
            Method::DELETE,
            &format!("/api/groups/{company}/subgroups/{team}"),
        )
        .await;
        let (status, _) = app.status_as(&token, Method::GET, &template).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _, _) = app
            .send(
                Method::PUT,
                &format!("{template}/permissions/groups/{}", Uuid::new_v4()),
                Some(json!({})),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "unknown group");
    }

    #[tokio::test]
    async fn a_manager_runs_the_template_but_does_not_give_rights() {
        let app = TestApp::new().await;
        let template = create_template(&app, &unique("tpl"), INVALID_CONFIG).await;
        let id = template["id"].as_str().unwrap();
        let path = format!("{}/{id}", templates(&app));
        wait_version(&app, id, 1, ended).await;

        let (eve, token) = app.user(&unique("eve"), "user").await;
        let (status, _) = app.status_as(&token, Method::GET, &path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "no right yet");
        let team = group_with(&app, &eve).await;
        let (status, _, _) = app
            .send(
                Method::PUT,
                &format!("{path}/permissions/groups/{team}"),
                Some(json!({ "role": "manage" })),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Managers see the template before any build succeeded, with its configuration.
        let (status, json) = app.status_as(&token, Method::GET, &path).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["access"]["level"], "manage");
        assert_eq!(json["packer_config"], INVALID_CONFIG);
        assert_eq!(json["latest_version"]["status"], "failed");
        let (status, log) = app
            .status_as(&token, Method::GET, &format!("{path}/versions/1/logs"))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(log["content"].as_str().unwrap().contains("packer"));

        let (status, _, _) = app
            .send_as(
                Some(&token),
                Method::PATCH,
                &path,
                Some(json!({ "packer_config": "edited by a manager" })),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _, version) = app
            .send_as(
                Some(&token),
                Method::POST,
                &format!("{path}/versions"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{version}");
        assert_eq!(version["packer_config"], "edited by a manager");
        wait_version(&app, id, 2, ended).await;
        let (status, _) = app
            .status_as(&token, Method::DELETE, &format!("{path}/versions/1"))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        // Rights and the template itself stay with admins.
        for (method, target, body) in [
            (
                Method::PATCH,
                path.clone(),
                Some(json!({ "visibility": "restricted" })),
            ),
            (Method::GET, format!("{path}/permissions"), None),
            (
                Method::PUT,
                format!("{path}/permissions/users/{eve}"),
                Some(json!({ "role": "use" })),
            ),
            (Method::DELETE, path.clone(), None),
        ] {
            let (status, _, _) = app
                .send_as(Some(&token), method.clone(), &target, body)
                .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {target}");
        }
    }

    #[tokio::test]
    async fn a_right_can_limit_the_vms_made_from_the_template() {
        let app = TestApp::new().await;
        let (id, _) = insert_template(&app, "created").await;
        let template = format!("{}/{id}", templates(&app));
        app.send(
            Method::PATCH,
            &template,
            Some(json!({ "visibility": "restricted" })),
        )
        .await;
        let (frank, token) = app.user(&unique("frank"), "user").await;
        let user_right = format!("{template}/permissions/users/{frank}");
        let (status, _, _) = app
            .send(Method::PUT, &user_right, Some(json!({ "max_vms": 1 })))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _, _) = app
            .send(Method::PUT, &user_right, Some(json!({ "max_vms": -1 })))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let create = |name: String| {
            let token = token.clone();
            let path = format!("{template}/vms");
            let app = &app;
            async move {
                app.send_as(
                    Some(&token),
                    Method::POST,
                    &path,
                    Some(json!({ "name": name, "memory_mb": 128, "vcpus": 1 })),
                )
                .await
            }
        };
        let (status, _, job) = create(unique("vm")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let job = app.wait_job(job["id"].as_str().unwrap()).await;
        assert_eq!(job["status"], "succeeded");
        let vm_id = job["result"]["id"].as_str().unwrap().to_owned();
        let (_, json) = app.status_as(&token, Method::GET, &template).await;
        assert_eq!(json["access"]["max_vms"], 1);
        assert_eq!(json["access"]["vm_count"], 1);

        let (status, _, json) = create(unique("vm")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{json}");
        assert!(json["content"].as_str().unwrap().contains("at most 1"));

        // The most generous right wins: a group of frank's allows 2.
        let team = group_with(&app, &frank).await;
        app.send(
            Method::PUT,
            &format!("{template}/permissions/groups/{team}"),
            Some(json!({ "max_vms": 2 })),
        )
        .await;
        let (status, _, job) = create(unique("vm")).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        app.wait_job(job["id"].as_str().unwrap()).await;
        let (status, _, _) = create(unique("vm")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (_, entries) = app
            .status_of(Method::GET, &format!("{template}/permissions"))
            .await;
        let entry = entries
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "user")
            .unwrap();
        assert_eq!(entry["vm_count"], 2);

        // Deleting a VM gives its slot back.
        let (status, _) = app.status_as(&token, Method::DELETE, &app.vm(&vm_id)).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _, _) = create(unique("vm")).await;
        assert_eq!(status, StatusCode::ACCEPTED);

        // Admins are not limited.
        let (status, _, _) = app
            .send(
                Method::POST,
                &format!("{template}/vms"),
                Some(json!({ "name": unique("vm"), "memory_mb": 128, "vcpus": 1, "owner_id": frank })),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn vm_creation_can_be_turned_off_for_a_template() {
        let app = TestApp::new().await;
        let (id, _) = insert_template(&app, "created").await;
        let template = format!("{}/{id}", templates(&app));
        let (_, token) = granted_user(&app).await;
        let vm = || json!({ "name": unique("vm"), "memory_mb": 128, "vcpus": 1 });

        let (_, json) = app.status_of(Method::GET, &template).await;
        assert_eq!(json["allow_vm_creation"], true, "allowed by default");
        let (status, _, json) = app
            .send(
                Method::PATCH,
                &template,
                Some(json!({ "allow_vm_creation": false })),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["allow_vm_creation"], false);

        // Still listed (the UI can show it as unavailable), but refused for everyone.
        let (_, listed) = app.status_as(&token, Method::GET, &templates(&app)).await;
        assert_eq!(listed[0]["allow_vm_creation"], false);
        let (status, _, _) = app
            .send_as(
                Some(&token),
                Method::POST,
                &format!("{template}/vms"),
                Some(vm()),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, _, _) = app
            .send(Method::POST, &format!("{template}/vms"), Some(vm()))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "admins too");

        let (status, _, _) = app
            .send(
                Method::PATCH,
                &template,
                Some(json!({ "allow_vm_creation": true })),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _, job) = app
            .send_as(
                Some(&token),
                Method::POST,
                &format!("{template}/vms"),
                Some(vm()),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(
            app.wait_job(job["id"].as_str().unwrap()).await["status"],
            "succeeded"
        );

        // It can also be chosen at creation.
        let mut body = template_body(&unique("tpl"), INVALID_CONFIG);
        body["allow_vm_creation"] = json!(false);
        let (status, _, json) = app.send(Method::POST, &templates(&app), Some(body)).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(json["allow_vm_creation"], false);
        wait_version(&app, json["id"].as_str().unwrap(), 1, ended).await;
    }

    // ---------- builds ----------

    #[tokio::test]
    async fn a_build_runs_in_the_background_and_keeps_its_log() {
        let app = TestApp::new().await;
        let name = unique("tpl");
        let template = create_template(&app, &name, INVALID_CONFIG).await;
        let id = template["id"].as_str().unwrap();
        assert_eq!(template["latest_version"]["version"], 1);
        assert_eq!(
            template["latest_version"]["volume"],
            format!("template-{name}-v1.qcow2")
        );
        assert!(matches!(
            template["status"].as_str(),
            Some("pending" | "in_progress")
        ));

        // The same name cannot be reused while the template exists.
        let (status, _, _) = app
            .send(
                Method::POST,
                &templates(&app),
                Some(template_body(&name, INVALID_CONFIG)),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);

        // An invalid configuration (or no packer at all) ends in `failed`, with the output.
        let version = wait_version(&app, id, 1, ended).await;
        assert_eq!(version["status"], "failed", "{version}");
        assert!(!version["error"].as_str().unwrap().is_empty());
        assert!(!volume_in_pool(&format!("template-{name}-v1.qcow2")));

        let log = logs(&app, id, 1, 0).await;
        assert_eq!(log["finished"], true);
        let content = log["content"].as_str().unwrap();
        assert!(content.contains("==> packer init"), "{content}");
        assert!(
            !content.contains("@@build"),
            "announcements stay out of the log"
        );
        let next = log["next_offset"].as_u64().unwrap() as usize;
        assert_eq!(logs(&app, id, 1, next).await["content"], "");
        let (status, _) = app
            .status_of(
                Method::GET,
                &format!(
                    "{}/{id}/versions/1/logs?offset={}",
                    templates(&app),
                    next + 1
                ),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn an_edit_is_built_as_a_new_version() {
        let app = TestApp::new().await;
        let template = create_template(&app, &unique("tpl"), INVALID_CONFIG).await;
        let id = template["id"].as_str().unwrap();
        wait_version(&app, id, 1, ended).await;

        let path = format!("{}/{id}", templates(&app));
        let new_name = unique("renamed");
        let (status, _, json) = app
            .send(
                Method::PATCH,
                &path,
                Some(json!({ "name": new_name, "packer_config": "still not packer" })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["packer_config"], "still not packer");

        let (status, headers, version) = app
            .send(Method::POST, &format!("{path}/versions"), None)
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{version}");
        assert_eq!(version["version"], 2);
        assert_eq!(version["packer_config"], "still not packer");
        assert_eq!(version["volume"], format!("template-{new_name}-v2.qcow2"));
        assert_eq!(headers[header::LOCATION], format!("{path}/versions/2"));
        wait_version(&app, id, 2, ended).await;

        // Version 1 keeps what it was built from.
        let (_, first) = app
            .status_of(Method::GET, &format!("{path}/versions/1"))
            .await;
        assert_eq!(first["packer_config"], INVALID_CONFIG);
    }

    #[tokio::test]
    async fn one_build_at_a_time_per_template() {
        let app = TestApp::new().await;
        let (id, _) = insert_template(&app, "in_progress").await;
        let (status, json) = app
            .status_of(Method::POST, &format!("{}/{id}/versions", templates(&app)))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "{json}");
    }

    #[tokio::test]
    async fn a_build_cut_by_a_restart_is_reported_as_failed() {
        let app = TestApp::new().await;
        let (id, _) = insert_template_at(&app, "in_progress", 0).await;
        let (status, json) = app
            .status_of(Method::GET, &format!("{}/{id}/versions/1", templates(&app)))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["status"], "failed");
        assert!(json["error"].as_str().unwrap().contains("interrupted"));
    }

    #[tokio::test]
    async fn a_stop_lets_packer_clean_up() {
        if !packer_installed() {
            eprintln!("packer is not installed: skipped");
            return;
        }
        let app = TestApp::new().await;
        let template = create_template(&app, &unique("tpl"), SLEEPING_CONFIG).await;
        let id = template["id"].as_str().unwrap();
        let process = wait_until_sleeping(&app, id).await;
        let work_dir = process
            .work_dir
            .expect("the script announced its directory");

        let (status, version) = app
            .status_of(Method::POST, &format!("{}/{id}/stop", templates(&app)))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{version}");
        assert_eq!(version["status"], "stopping");
        let version = wait_version(&app, id, 1, ended).await;
        assert_eq!(version["status"], "cancelled", "{version}");
        assert!(!std::path::Path::new(&work_dir).exists());
        assert!(!group_alive(process.group.unwrap()));

        let (status, _) = app
            .status_of(Method::POST, &format!("{}/{id}/stop", templates(&app)))
            .await;
        assert_eq!(status, StatusCode::CONFLICT, "nothing left to stop");
    }

    #[tokio::test]
    async fn a_forced_stop_kills_the_build_at_once() {
        if !packer_installed() {
            eprintln!("packer is not installed: skipped");
            return;
        }
        let app = TestApp::new().await;
        let template = create_template(&app, &unique("tpl"), SLEEPING_CONFIG).await;
        let id = template["id"].as_str().unwrap();
        let process = wait_until_sleeping(&app, id).await;

        let (status, _) = app
            .status_of(
                Method::POST,
                &format!("{}/{id}/stop?force=true", templates(&app)),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let version = wait_version(&app, id, 1, ended).await;
        assert_eq!(version["status"], "cancelled");
        assert!(!group_alive(process.group.unwrap()));
        assert!(!std::path::Path::new(&process.work_dir.unwrap()).exists());
        let log = logs(&app, id, 1, 0).await;
        assert!(log["content"].as_str().unwrap().contains("started"));
    }

    // ---------- VMs ----------

    #[tokio::test]
    async fn a_vm_created_from_a_template_belongs_to_its_creator_and_has_its_own_disk() {
        let app = TestApp::new().await;
        let (id, template_volume) = insert_template(&app, "created").await;
        let (user_id, token) = granted_user(&app).await;
        let name = unique("vm");
        let path = format!("{}/{id}/vms", templates(&app));
        let (status, headers, job) = app
            .send_as(
                Some(&token),
                Method::POST,
                &path,
                Some(json!({ "name": name, "memory_mb": 128, "vcpus": 1 })),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{job}");
        let job_id = job["id"].as_str().unwrap();
        assert_eq!(headers[header::LOCATION], format!("/api/jobs/{job_id}"));

        let (status, _) = app
            .status_as(&token, Method::GET, &format!("/api/jobs/{job_id}"))
            .await;
        assert_eq!(status, StatusCode::OK, "the creator follows the job");
        let job = app.wait_job(job_id).await;
        assert_eq!(job["status"], "succeeded", "{job}");
        assert_eq!(job["result"]["access"], "owner");
        let vm_id = job["result"]["id"].as_str().unwrap();

        let (status, vm) = app.status_as(&token, Method::GET, &app.vm(vm_id)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(vm["name"], name.as_str());
        let owner: String = sqlx::query_scalar("SELECT owner_id FROM vm_owners WHERE vm_id = $1")
            .bind(vm_id)
            .fetch_one(&app.state.db)
            .await
            .unwrap();
        assert_eq!(owner, user_id);
        assert!(volume_in_pool(&format!("{name}.qcow2")));
        assert!(volume_in_pool(&template_volume));

        // The name is now taken: refused before any job starts.
        let (status, _, _) = app
            .send_as(
                Some(&token),
                Method::POST,
                &path,
                Some(json!({ "name": name, "memory_mb": 128, "vcpus": 1 })),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn a_vm_uses_the_latest_built_version_unless_told_otherwise() {
        let app = TestApp::new().await;
        let (id, first_volume) = insert_template(&app, "created").await;
        let name = first_volume
            .strip_prefix("template-")
            .unwrap()
            .strip_suffix("-v1.qcow2")
            .unwrap()
            .to_owned();
        insert_version(&app, &id, &name, 2, "created", live()).await;
        insert_version(&app, &id, &name, 3, "failed", live()).await;
        let (_, template) = app
            .status_of(Method::GET, &format!("{}/{id}", templates(&app)))
            .await;
        assert_eq!(template["default_version"], 2);
        assert_eq!(template["status"], "failed", "the latest build failed");

        let path = format!("{}/{id}/vms", templates(&app));
        let (status, _, job) = app
            .send(
                Method::POST,
                &path,
                Some(json!({ "name": unique("vm"), "memory_mb": 128, "vcpus": 1, "version": 1 })),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(
            app.wait_job(job["id"].as_str().unwrap()).await["status"],
            "succeeded"
        );
        let (status, _, _) = app
            .send(
                Method::POST,
                &path,
                Some(json!({ "name": unique("vm"), "memory_mb": 128, "vcpus": 1, "version": 3 })),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "a failed version is not usable"
        );
    }

    #[tokio::test]
    async fn creating_a_vm_from_a_template_needs_access_and_quota() {
        let app = TestApp::new().await;
        let (id, _) = insert_template(&app, "created").await;
        let path = format!("{}/{id}/vms", templates(&app));
        let body = json!({ "name": unique("vm"), "memory_mb": 128, "vcpus": 1 });

        let (_, stranger) = app.user(&unique("user"), "user").await;
        let (status, _, _) = app
            .send_as(Some(&stranger), Method::POST, &path, Some(body.clone()))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "no right on the template");

        // The template disk counts in the quota of the new VM.
        let (user_id, token) = granted_user(&app).await;
        sqlx::query("UPDATE users SET max_disk_gb = 0 WHERE id = $1")
            .bind(&user_id)
            .execute(&app.state.db)
            .await
            .unwrap();
        let (status, _, json) = app
            .send_as(Some(&token), Method::POST, &path, Some(body.clone()))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(json["content"].as_str().unwrap().contains("max_disk_gb"));

        // Only an admin picks another owner.
        let mut for_someone = body.clone();
        for_someone["owner_id"] = json!(user_id);
        let (_, other) = granted_user(&app).await;
        let (status, _, _) = app
            .send_as(Some(&other), Method::POST, &path, Some(for_someone))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // A template without a built version cannot be used, even by an admin.
        let (failed, _) = insert_template(&app, "failed").await;
        let (status, _, _) = app
            .send(
                Method::POST,
                &format!("{}/{failed}/vms", templates(&app)),
                Some(body),
            )
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    // ---------- deletion ----------

    #[tokio::test]
    async fn deleting_removes_the_disks_but_not_while_building() {
        let app = TestApp::new().await;
        let (id, first) = insert_template(&app, "created").await;
        let name = first
            .strip_prefix("template-")
            .unwrap()
            .strip_suffix("-v1.qcow2")
            .unwrap()
            .to_owned();
        let second = insert_version(&app, &id, &name, 2, "created", live()).await;
        let path = format!("{}/{id}", templates(&app));

        let (status, _) = app
            .status_of(Method::DELETE, &format!("{path}/versions/1"))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(!volume_in_pool(&first));
        assert!(volume_in_pool(&second));

        let (status, _) = app.status_of(Method::DELETE, &path).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(!volume_in_pool(&second));
        let (status, _) = app.status_of(Method::GET, &path).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (building, _) = insert_template(&app, "in_progress").await;
        let building = format!("{}/{building}", templates(&app));
        let (status, _) = app.status_of(Method::DELETE, &building).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, _) = app
            .status_of(Method::DELETE, &format!("{building}/versions/1"))
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }
}
