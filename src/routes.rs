use axum::{
    Router,
    routing::{get, post, put},
};

use crate::{
    audit, common::ping, groups, hypervisors, infrastructure, jobs, labs, shares,
    state::SharedState, templates, users, vm_options, vms, vnc,
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
        .route("/api/me/groups", get(groups::my_groups))
        .route("/api/me/lab-instances", get(labs::my_instances))
        .route("/api/users", get(users::list_users).post(users::create_user))
        .route(
            "/api/users/{user_id}",
            get(users::get_user)
                .patch(users::update_user)
                .delete(users::delete_user),
        )
        .route("/api/users/{user_id}/password", put(users::reset_password))
        .route("/api/users/{user_id}/quota", put(users::set_quota))
        .route("/api/users/{user_id}/groups", get(groups::user_groups))
        // Groups (admin, except /api/me/groups)
        .route("/api/groups", get(groups::list).post(groups::create))
        .route(
            "/api/groups/{group_id}",
            get(groups::get)
                .patch(groups::update)
                .delete(groups::delete),
        )
        .route("/api/groups/{group_id}/members", get(groups::members))
        .route(
            "/api/groups/{group_id}/members/{user_id}",
            put(groups::add_member).delete(groups::remove_member),
        )
        .route(
            "/api/groups/{group_id}/subgroups/{child_id}",
            put(groups::add_subgroup).delete(groups::remove_subgroup),
        )
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
        .route(
            "/api/hypervisors/{hypervisor_id}/templates",
            get(templates::list).post(templates::create),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/templates/{template_id}",
            get(templates::get)
                .patch(templates::update)
                .delete(templates::delete),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/templates/{template_id}/versions",
            get(templates::list_versions).post(templates::rebuild),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/templates/{template_id}/versions/{version}",
            get(templates::get_version).delete(templates::delete_version),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/templates/{template_id}/versions/{version}/logs",
            get(templates::logs),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/templates/{template_id}/stop",
            post(templates::stop),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/templates/{template_id}/permissions",
            get(templates::list_permissions),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/templates/{template_id}/permissions/users/{user_id}",
            put(templates::set_user_permission).delete(templates::remove_user_permission),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/templates/{template_id}/permissions/groups/{group_id}",
            put(templates::set_group_permission).delete(templates::remove_group_permission),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/templates/{template_id}/vms",
            post(templates::create_vm),
        )
        // Labs
        .route(
            "/api/hypervisors/{hypervisor_id}/labs",
            get(labs::list).post(labs::create),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/labs/{lab_id}",
            get(labs::get).patch(labs::update).delete(labs::delete),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/networks",
            post(labs::add_network),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/networks/{network_id}",
            axum::routing::delete(labs::remove_network),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/vms",
            post(labs::add_vm),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/vms/{lab_vm_id}",
            put(labs::update_vm).delete(labs::remove_vm),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/permissions",
            get(labs::list_permissions),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/permissions/users/{user_id}",
            put(labs::set_user_permission).delete(labs::remove_user_permission),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/permissions/groups/{group_id}",
            put(labs::set_group_permission).delete(labs::remove_group_permission),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/instances",
            get(labs::list_instances).post(labs::create_instance),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/instances/{instance_id}",
            get(labs::get_instance).delete(labs::delete_instance),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/instances/{instance_id}/start",
            post(labs::start_instance),
        )
        .route(
            "/api/hypervisors/{hypervisor_id}/labs/{lab_id}/instances/{instance_id}/stop",
            post(labs::stop_instance),
        )
        .route("/api/jobs/{job_id}", get(jobs::get))
        .with_state(state)
}
