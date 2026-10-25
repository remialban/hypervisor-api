use utoipa::{
    Modify, OpenApi,
    openapi::security::{
        Flow, HttpAuthScheme, HttpBuilder, OAuth2, Password, Scopes, SecurityRequirement,
        SecurityScheme,
    },
};

use crate::{
    access::{Quota, Usage},
    audit, common, groups, hypervisors, infrastructure, jobs, labs,
    models::{
        CloneVm, CreateHypervisor, CreateVm, Disk, DiskRequest, DiskSpec, DiskUpdateRequest,
        Hypervisor, Message, Network, NetworkRequest, StoragePool, StoragePoolRequest,
        UpdateHypervisor, VirtualMachine, VncResponse, VolumeRef,
    },
    shares, templates, users, vm_options, vms, vnc,
};

/// Every route needs a token, except the ones marked `security(())`. Swagger UI's
/// "Authorize" button offers the OAuth2 login form or a pasted Bearer token.
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .build(),
            ),
        );
        components.add_security_scheme(
            "oauth2",
            SecurityScheme::OAuth2(OAuth2::new([Flow::Password(Password::new(
                "/api/auth/token",
                Scopes::new(),
            ))])),
        );
        openapi.security = Some(vec![
            SecurityRequirement::new("bearer", Vec::<String>::new()),
            SecurityRequirement::new("oauth2", Vec::<String>::new()),
        ]);
    }
}

#[derive(OpenApi)]
#[openapi(
    modifiers(&SecurityAddon),
    info(
        title = "KVM Multi-Hypervisor API",
        version = "0.1.0",
        description = "API for managing KVM hypervisors, virtual machines, disks, networks and VNC"
    ),
    paths(
        common::ping,
        hypervisors::list,
        hypervisors::create,
        hypervisors::get,
        hypervisors::update,
        hypervisors::delete,
        hypervisors::list_access,
        hypervisors::grant_access,
        hypervisors::revoke_access,
        users::token,
        users::login_json,
        users::refresh_json,
        users::logout,
        users::register,
        users::auth_config,
        users::me,
        users::change_password,
        users::list_users,
        users::create_user,
        users::get_user,
        users::update_user,
        users::delete_user,
        users::reset_password,
        users::set_quota,
        users::get_settings,
        users::put_settings,
        audit::list,
        groups::list,
        groups::create,
        groups::get,
        groups::update,
        groups::delete,
        groups::members,
        groups::add_member,
        groups::remove_member,
        groups::add_subgroup,
        groups::remove_subgroup,
        groups::my_groups,
        groups::user_groups,
        shares::list,
        shares::create,
        shares::revoke,
        shares::change_owner,
        vms::list,
        vms::get,
        vms::create,
        vms::start,
        vms::stop,
        vms::clone,
        vms::delete,
        infrastructure::list_disks,
        infrastructure::create_disk,
        infrastructure::list_pools,
        infrastructure::create_pools,
        infrastructure::get_disk,
        infrastructure::update_disk,
        infrastructure::delete_disk,
        infrastructure::list_networks,
        infrastructure::create_network,
        vnc::connect,
        templates::list,
        templates::get,
        templates::create,
        templates::update,
        templates::delete,
        templates::list_versions,
        templates::rebuild,
        templates::get_version,
        templates::delete_version,
        templates::logs,
        templates::stop,
        templates::list_permissions,
        templates::set_user_permission,
        templates::set_group_permission,
        templates::remove_user_permission,
        templates::remove_group_permission,
        labs::list,
        labs::create,
        labs::get,
        labs::update,
        labs::delete,
        labs::add_network,
        labs::remove_network,
        labs::add_vm,
        labs::update_vm,
        labs::remove_vm,
        labs::list_permissions,
        labs::set_user_permission,
        labs::set_group_permission,
        labs::remove_user_permission,
        labs::remove_group_permission,
        labs::list_instances,
        labs::create_instance,
        labs::get_instance,
        labs::start_instance,
        labs::stop_instance,
        labs::delete_instance,
        labs::my_instances,
        templates::create_vm,
        jobs::get,
        vm_options::get
    ),
    components(schemas(
        Message,
        Hypervisor,
        CreateHypervisor,
        UpdateHypervisor,
        CreateVm,
        DiskSpec,
        VolumeRef,
        VirtualMachine,
        Disk,
        DiskRequest,
        DiskUpdateRequest,
        StoragePool,
        StoragePoolRequest,
        Network,
        NetworkRequest,
        VncResponse,
        CloneVm,
        jobs::Job,
        jobs::JobStatus,
        vm_options::VmOptions,
        vm_options::PoolOption,
        vm_options::VolumeOption,
        vm_options::VolumeKind,
        vm_options::NetworkOption,
        vm_options::Range,
        vm_options::Limits,
        vm_options::Defaults,
        hypervisors::AccessEntry,
        users::TokenResponse,
        users::LoginRequest,
        users::RefreshRequest,
        users::TokenForm,
        users::AuthConfig,
        users::UserView,
        users::ChangePassword,
        users::CreateUser,
        users::UpdateUser,
        users::ResetPassword,
        users::Settings,
        Quota,
        Usage,
        audit::AuditEntry,
        groups::Group,
        groups::GroupDetail,
        groups::GroupMember,
        groups::GroupRef,
        groups::EffectiveMember,
        groups::UserGroup,
        groups::CreateGroup,
        groups::UpdateGroup,
        shares::ShareView,
        shares::CreateShare,
        shares::ChangeOwner,
        templates::Template,
        templates::TemplateStatus,
        templates::Visibility,
        templates::TemplateVersion,
        templates::BuildLog,
        templates::TemplateRole,
        templates::AccessLevel,
        templates::TemplateAccess,
        templates::PrincipalKind,
        templates::TemplatePermission,
        templates::SetPermission,
        labs::NetworkKind,
        labs::VmRole,
        labs::InstanceStatus,
        labs::LabAccess,
        labs::Lab,
        labs::LabDetail,
        labs::LabNetwork,
        labs::LabVm,
        labs::CreateLab,
        labs::UpdateLab,
        labs::CreateLabNetwork,
        labs::LabVmBody,
        labs::SetLabPermission,
        labs::LabPermission,
        labs::CreateInstance,
        labs::LabInstance,
        labs::InstanceNetwork,
        labs::InstanceVm,
        labs::MyInstance,
        templates::CreateTemplate,
        templates::UpdateTemplate,
        templates::CreateVmFromTemplate
    )),
    tags(
        (name = "Auth", description = "Login (OAuth2 password grant, JWT) and registration"),
        (name = "Users", description = "Accounts, passwords and quotas"),
        (name = "Groups", description = "Groups of users and nested groups (admin; users see their own)"),
        (name = "Settings", description = "Global settings (admin)"),
        (name = "Audit", description = "Journal of who did what (admin)"),
        (name = "Sharing", description = "Lending a VM to another user"),
        (name = "Hypervisors", description = "Hypervisor management"),
        (name = "VMs", description = "Virtual machine management"),
        (name = "Jobs", description = "Background operations"),
        (name = "Templates", description = "Disk images built with Packer, and VMs created from them"),
        (name = "Labs", description = "Sets of VMs from templates and networks (OPNsense routers...), deployed as instances"),
        (name = "Infrastructure", description = "Disks, networks and VNC")
    )
)]
pub struct ApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> serde_json::Value {
        serde_json::to_value(ApiDoc::openapi()).unwrap()
    }

    #[test]
    fn the_spec_declares_bearer_and_oauth2_for_the_whole_api() {
        let spec = spec();
        let schemes = &spec["components"]["securitySchemes"];
        assert_eq!(schemes["bearer"]["scheme"], "bearer");
        assert_eq!(
            schemes["oauth2"]["flows"]["password"]["tokenUrl"],
            "/api/auth/token"
        );
        assert_eq!(spec["security"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn public_routes_are_marked_as_such() {
        let spec = spec();
        for (path, method) in [
            ("/ping", "get"),
            ("/api/auth/token", "post"),
            ("/api/auth/login", "post"),
            ("/api/auth/refresh", "post"),
            ("/api/auth/register", "post"),
            ("/api/auth/config", "get"),
        ] {
            assert_eq!(
                spec["paths"][path][method]["security"],
                serde_json::json!([{}]),
                "{method} {path}"
            );
        }
        assert!(spec["paths"]["/api/hypervisors"]["get"]["security"].is_null());
    }

    #[test]
    fn every_route_of_the_router_is_documented() {
        let spec = spec();
        let documented = spec["paths"].as_object().unwrap();
        for path in [
            "/api/me",
            "/api/users",
            "/api/users/{user_id}/quota",
            "/api/settings",
            "/api/audit",
            "/api/hypervisors/{id}/access/{user_id}",
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/shares",
            "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/owner",
        ] {
            assert!(documented.contains_key(path), "{path}");
        }
    }
}
