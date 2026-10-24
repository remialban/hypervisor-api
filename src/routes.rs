use axum::{
    routing::{get, post, put},
    Router,
};

use crate::{
    audit,
    common::ping,
    hypervisors,
    infrastructure,
    jobs,
    shares,
    state::SharedState,
    users,
    vm_options,
    vms,
    vnc,
};

pub fn create_routes(state: SharedState) -> Router {
    Router::new()
        .route("/ping", get(ping))
        // Authentication (public)
        .route("/api/auth/token", post(users::token))
        .route("/api/auth/login", post(users::login_json))
        .route("/api/auth/refresh", post(users::refresh_json))
        .route("/api/auth/logout", post(users::logout))
        .route("/api/auth/register", post(users::register))
        .route("/api/auth/config", get(users::auth_config))
        // Accounts
        .route("/api/me", get(users::me))
        .route("/api/me/password", put(users::change_password))
        .route("/api/users", get(users::list_users).post(users::create_user))
        .route(
            "/api/users/{user_id}",
            get(users::get_user)
                .patch(users::update_user)
                .delete(users::delete_user),
        )
        .route("/api/users/{user_id}/password", put(users::reset_password))
        .route("/api/users/{user_id}/quota", put(users::set_quota))
        .route("/api/settings", get(users::get_settings).put(users::put_settings))
        .route("/api/audit", get(audit::list))
        .route(
            "/api/hypervisors",
            get(hypervisors::list).post(hypervisors::create),
        )
        .route(
            "/api/hypervisors/{id}",
            get(hypervisors::get)
                .patch(hypervisors::update)
                .delete(hypervisors::delete),
        )
        .route(
            "/api/hypervisors/{id}/access",
            get(hypervisors::list_access),
        )
        .route(
            "/api/hypervisors/{id}/access/{user_id}",
            put(hypervisors::grant_access).delete(hypervisors::revoke_access),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms",
            get(vms::list).post(vms::create),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/shares",
            get(shares::list).post(shares::create),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/shares/{user_id}",
            axum::routing::delete(shares::revoke),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/owner",
            put(shares::change_owner),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}",
            get(vms::get).delete(vms::delete),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/start",
            post(vms::start),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/stop",
            post(vms::stop),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/clone",
            post(vms::clone),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/disks",
            get(infrastructure::list_disks).post(infrastructure::create_disk),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/pools",
            get(infrastructure::list_pools).post(infrastructure::create_pools),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/disks/{disk_id}",
            get(infrastructure::get_disk)
                .patch(infrastructure::update_disk)
                .delete(infrastructure::delete_disk),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/networks",
            get(infrastructure::list_networks).post(infrastructure::create_network),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/vnc",
            get(vnc::connect),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/vm-options",
            get(vm_options::get),
        )
        .route("/api/jobs/{job_id}", get(jobs::get))
        .with_state(state)
}
