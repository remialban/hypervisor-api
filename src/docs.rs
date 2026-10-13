use utoipa::OpenApi;

use crate::{
    common,
    hypervisors,
    infrastructure,
    jobs,
    models::{
        CloneVm, CreateHypervisor, CreateVm, Disk, DiskRequest, Hypervisor, Message, Network,
        NetworkRequest, UpdateHypervisor, VirtualMachine, VncResponse,
    },
    vms,
    vnc,
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
        vnc::connect,
        jobs::get
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
        NetworkRequest,
        VncResponse,
        CloneVm,
        jobs::Job,
        jobs::JobStatus
    )),
    tags(
        (name = "Hypervisors", description = "Hypervisor management"),
        (name = "VMs", description = "Virtual machine management"),
        (name = "Jobs", description = "Background operations"),
        (name = "Infrastructure", description = "Disks, networks and VNC")
    )
)]
pub struct ApiDoc;