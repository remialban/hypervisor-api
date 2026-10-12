use axum::{extract::Path, http::StatusCode, Json};

use crate::{common::not_implemented, models::Message};

type ApiResponse = (StatusCode, Json<Message>);

#[utoipa::path(
    get,
    path = "/api/hypervisors/{hypervisor_id}/vms/{vm_id}/vnc",
    params(("hypervisor_id" = String, Path), ("vm_id" = String, Path)),
    responses((status = 501, body = Message)),
    tag = "VNC"
)]
pub async fn connect(
    Path((_hypervisor_id, _vm_id)): Path<(String, String)>,
) -> ApiResponse {
    not_implemented("VNC access will be connected to libvirt")
}
