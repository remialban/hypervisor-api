use utoipa::OpenApi;

use crate::{
    common,
    hypervisors,
    infrastructure,
    models::{
        CreateHypervisor, CreateVm, Disk, DiskRequest, Hypervisor, Message, Network,
        NetworkRequest, UpdateHypervisor, VirtualMachine,
    },
    vms,
};

#[derive(OpenApi)]
#[openapi(
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
        vms::list,
        vms::get,
        vms::create,
        vms::start,
        vms::stop,
        vms::clone,
        vms::delete,
        infrastructure::list_disks,
        infrastructure::create_disk,
        infrastructure::get_disk,
        infrastructure::update_disk,
        infrastructure::delete_disk,
        infrastructure::list_networks,
        infrastructure::create_network,
        infrastructure::vnc
    ),
    components(schemas(
        Message,
        Hypervisor,
        CreateHypervisor,
        UpdateHypervisor,
        CreateVm,
        VirtualMachine,
        Disk,
        DiskRequest,
        Network,
        NetworkRequest
    )),
    tags(
        (name = "Hypervisors", description = "Hypervisor management"),
        (name = "VMs", description = "Virtual machine management"),
        (name = "Infrastructure", description = "Disks, networks and VNC")
    )
)]
pub struct ApiDoc;