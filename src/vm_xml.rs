use std::collections::HashMap;

use xmltree::{Element, XMLNode};

use crate::errors::ApiError;

/// A VM name ends up in libvirt and in disk file names: keep it to a safe alphabet.
pub fn validate_name(name: &str) -> Result<(), ApiError> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with('.');
    if valid {
        Ok(())
    } else {
        Err(ApiError::BadRequest(
            "VM name must be 1-64 characters: letters, digits, '-', '_' or '.' (not leading)"
                .into(),
        ))
    }
}

/// File paths of the disks (not cdroms) of a domain definition, in order.
pub fn disk_sources(xml: &str) -> Result<Vec<String>, ApiError> {
    let root = parse(xml)?;
    let mut paths = Vec::new();
    for disk in disks(&root) {
        if let Some(path) = disk
            .get_child("source")
            .and_then(|s| s.attributes.get("file"))
        {
            paths.push(path.clone());
        }
    }
    Ok(paths)
}

/// A disk file attached to a new VM.
pub struct DiskAttachment {
    pub path: String,
    /// `qcow2`, `raw`... as reported by libvirt for the volume.
    pub format: String,
}

/// Everything needed to describe a new VM. Names and paths are escaped by the XML writer.
pub struct DomainSpec {
    pub name: String,
    pub memory_mb: u64,
    pub vcpus: u64,
    /// True on a real QEMU/KVM hypervisor (enables host CPU passthrough, q35, x86_64).
    pub kvm: bool,
    pub uefi: bool,
    pub disks: Vec<DiskAttachment>,
    /// Path of the installation ISO, attached as a CD-ROM and booted first.
    pub iso_path: Option<String>,
    pub network: String,
    /// Address the VNC display listens on.
    pub vnc_listen: String,
}

fn element(name: &str, attributes: &[(&str, &str)], children: Vec<Element>) -> Element {
    let mut element = Element::new(name);
    for (key, value) in attributes {
        element
            .attributes
            .insert(key.to_string(), value.to_string());
    }
    element.children = children.into_iter().map(XMLNode::Element).collect();
    element
}

fn text_element(name: &str, attributes: &[(&str, &str)], text: &str) -> Element {
    let mut element = element(name, attributes, vec![]);
    element.children = vec![XMLNode::Text(text.to_string())];
    element
}

/// Definition of a new VM: virtio devices, VNC display, one NIC on a libvirt network.
pub fn new_domain_xml(spec: &DomainSpec) -> Result<String, ApiError> {
    let mut os_type = text_element("type", &[], "hvm");
    if spec.kvm {
        os_type.attributes.insert("arch".into(), "x86_64".into());
        os_type.attributes.insert("machine".into(), "q35".into());
    }
    let mut os = element("os", &[], vec![os_type]);
    if spec.uefi {
        os.attributes.insert("firmware".into(), "efi".into());
    }
    if spec.iso_path.is_some() {
        os.children.push(XMLNode::Element(element(
            "boot",
            &[("dev", "cdrom")],
            vec![],
        )));
    }
    os.children
        .push(XMLNode::Element(element("boot", &[("dev", "hd")], vec![])));

    let mut devices = Element::new("devices");
    for (index, disk) in spec.disks.iter().enumerate() {
        let target = format!("vd{}", (b'a' + index as u8) as char);
        devices.children.push(XMLNode::Element(element(
            "disk",
            &[("type", "file"), ("device", "disk")],
            vec![
                element(
                    "driver",
                    &[("name", "qemu"), ("type", &disk.format)],
                    vec![],
                ),
                element("source", &[("file", &disk.path)], vec![]),
                element("target", &[("dev", &target), ("bus", "virtio")], vec![]),
            ],
        )));
    }
    if let Some(iso) = &spec.iso_path {
        devices.children.push(XMLNode::Element(element(
            "disk",
            &[("type", "file"), ("device", "cdrom")],
            vec![
                element("driver", &[("name", "qemu"), ("type", "raw")], vec![]),
                element("source", &[("file", iso)], vec![]),
                element("target", &[("dev", "sda"), ("bus", "sata")], vec![]),
                element("readonly", &[], vec![]),
            ],
        )));
    }
    let listen = &spec.vnc_listen;
    let extra_devices = vec![
        element(
            "interface",
            &[("type", "network")],
            vec![
                element("source", &[("network", &spec.network)], vec![]),
                element("model", &[("type", "virtio")], vec![]),
            ],
        ),
        element(
            "graphics",
            &[
                ("type", "vnc"),
                ("port", "-1"),
                ("autoport", "yes"),
                ("listen", listen),
            ],
            vec![element(
                "listen",
                &[("type", "address"), ("address", listen)],
                vec![],
            )],
        ),
        element(
            "video",
            &[],
            vec![element("model", &[("type", "virtio")], vec![])],
        ),
        element("memballoon", &[("model", "virtio")], vec![]),
        element(
            "rng",
            &[("model", "virtio")],
            vec![text_element(
                "backend",
                &[("model", "random")],
                "/dev/urandom",
            )],
        ),
    ];
    devices
        .children
        .extend(extra_devices.into_iter().map(XMLNode::Element));

    let mut root = element(
        "domain",
        &[("type", if spec.kvm { "kvm" } else { "test" })],
        vec![],
    );
    root.children
        .push(XMLNode::Element(text_element("name", &[], &spec.name)));
    root.children.push(XMLNode::Element(text_element(
        "memory",
        &[("unit", "MiB")],
        &spec.memory_mb.to_string(),
    )));
    root.children.push(XMLNode::Element(text_element(
        "vcpu",
        &[],
        &spec.vcpus.to_string(),
    )));
    root.children.push(XMLNode::Element(os));
    if spec.kvm {
        root.children.push(XMLNode::Element(element(
            "cpu",
            &[("mode", "host-passthrough")],
            vec![],
        )));
        root.children.push(XMLNode::Element(element(
            "features",
            &[],
            vec![element("acpi", &[], vec![]), element("apic", &[], vec![])],
        )));
    }
    root.children.push(XMLNode::Element(element(
        "clock",
        &[("offset", "utc")],
        vec![],
    )));
    root.children.push(XMLNode::Element(devices));

    let mut out = Vec::new();
    root.write(&mut out)
        .map_err(|e| ApiError::Internal(format!("Could not write the VM definition: {e}")))?;
    String::from_utf8(out)
        .map_err(|_| ApiError::Internal("VM definition is not valid UTF-8".into()))
}

/// Definition of a new thin-provisioned qcow2 volume.
pub fn new_disk_volume_xml(name: &str, size_gb: u64) -> Result<String, ApiError> {
    let root = element(
        "volume",
        &[],
        vec![
            text_element("name", &[], name),
            text_element("capacity", &[("unit", "GiB")], &size_gb.to_string()),
            element(
                "target",
                &[],
                vec![element("format", &[("type", "qcow2")], vec![])],
            ),
        ],
    );
    let mut out = Vec::new();
    root.write(&mut out)
        .map_err(|e| ApiError::Internal(format!("Could not write the volume definition: {e}")))?;
    String::from_utf8(out)
        .map_err(|_| ApiError::Internal("Volume definition is not valid UTF-8".into()))
}

/// Whether the definition has an NVRAM element (UEFI variables stored in a file).
pub fn has_nvram(xml: &str) -> Result<bool, ApiError> {
    let root = parse(xml)?;
    Ok(root
        .get_child("os")
        .and_then(|os| os.get_child("nvram"))
        .is_some())
}

/// Format of a volume (`qcow2`, `raw`, `iso`...) from its definition, `unknown` if absent.
pub fn volume_format(volume_xml: &str) -> Result<String, ApiError> {
    let root = parse(volume_xml)?;
    Ok(root
        .get_child("target")
        .and_then(|target| target.get_child("format"))
        .and_then(|format| format.attributes.get("type"))
        .cloned()
        .unwrap_or_else(|| "unknown".into()))
}

/// Forward mode (`nat`, `route`...) of a network definition, `None` for an isolated one.
pub fn network_forward_mode(network_xml: &str) -> Result<Option<String>, ApiError> {
    let root = parse(network_xml)?;
    Ok(root.get_child("forward").map(|forward| {
        forward
            .attributes
            .get("mode")
            .cloned()
            .unwrap_or_else(|| "nat".into())
    }))
}

/// Turns the definition of a stopped VM into the definition of its clone:
/// new name, no UUID or MAC address (libvirt generates them), no NVRAM path
/// (libvirt builds one from the name) and the disks replaced via `new_disks`
/// (old path -> new path).
pub fn clone_definition(
    xml: &str,
    new_name: &str,
    new_disks: &HashMap<String, String>,
) -> Result<String, ApiError> {
    let mut root = parse(xml)?;

    root.take_child("uuid");
    root.get_mut_child("name")
        .ok_or_else(|| invalid_xml("missing <name>"))?
        .children = vec![XMLNode::Text(new_name.to_string())];

    if let Some(nvram) = root
        .get_mut_child("os")
        .and_then(|os| os.get_mut_child("nvram"))
    {
        nvram.children.clear();
    }

    if let Some(devices) = root.get_mut_child("devices") {
        for node in devices.children.iter_mut() {
            let XMLNode::Element(device) = node else {
                continue;
            };
            match device.name.as_str() {
                "interface" => {
                    device.take_child("mac");
                }
                "disk" if is_file_disk(device) => {
                    let source = device
                        .get_mut_child("source")
                        .ok_or_else(|| invalid_xml("disk without <source>"))?;
                    let old = source.attributes.get("file").cloned().unwrap_or_default();
                    let new = new_disks
                        .get(&old)
                        .ok_or_else(|| invalid_xml("disk without a cloned counterpart"))?;
                    source.attributes.insert("file".into(), new.clone());
                }
                _ => {}
            }
        }
    }

    let mut out = Vec::new();
    root.write(&mut out)
        .map_err(|e| ApiError::Internal(format!("Could not write the VM definition: {e}")))?;
    String::from_utf8(out)
        .map_err(|_| ApiError::Internal("VM definition is not valid UTF-8".into()))
}

/// Definition of the volume that will receive a copy of a source volume: same format
/// and capacity, but a new name and no identity (key, path) nor backing file, so the
/// copy is a standalone full image.
pub fn new_volume_xml(source_volume_xml: &str, new_name: &str) -> Result<String, ApiError> {
    let mut root = parse(source_volume_xml)?;
    for child in ["key", "allocation", "physical", "backingStore"] {
        root.take_child(child);
    }
    root.get_mut_child("name")
        .ok_or_else(|| invalid_xml("volume without <name>"))?
        .children = vec![XMLNode::Text(new_name.to_string())];
    if let Some(target) = root.get_mut_child("target") {
        target.take_child("path");
        target.take_child("timestamps");
    }
    let mut out = Vec::new();
    root.write(&mut out)
        .map_err(|e| ApiError::Internal(format!("Could not write the volume definition: {e}")))?;
    String::from_utf8(out)
        .map_err(|_| ApiError::Internal("Volume definition is not valid UTF-8".into()))
}

fn parse(xml: &str) -> Result<Element, ApiError> {
    Element::parse(xml.as_bytes()).map_err(|e| {
        eprintln!("Invalid domain XML: {e}");
        invalid_xml("could not parse the definition")
    })
}

fn invalid_xml(reason: &str) -> ApiError {
    ApiError::Internal(format!("Unsupported VM definition: {reason}"))
}

fn is_file_disk(disk: &Element) -> bool {
    disk.attributes.get("device").map(String::as_str) == Some("disk")
        && disk.attributes.get("type").map(String::as_str) == Some("file")
}

fn disks(root: &Element) -> impl Iterator<Item = &Element> {
    root.get_child("devices")
        .into_iter()
        .flat_map(|devices| devices.children.iter())
        .filter_map(|node| node.as_element())
        .filter(|e| e.name == "disk" && is_file_disk(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = "<domain type='kvm'>
  <name>source</name>
  <uuid>6c6ba5fd-737f-4716-96ef-48eff5d44a0b</uuid>
  <os><type arch='x86_64'>hvm</type><nvram template='/t.fd' format='raw'>/var/lib/libvirt/qemu/nvram/source_VARS.fd</nvram></os>
  <devices>
    <disk type='file' device='disk'><source file='/mnt/vm/a.qcow2'/><target dev='vda'/></disk>
    <disk type='file' device='cdrom'><source file='/iso/install.iso'/><target dev='sda'/></disk>
    <interface type='network'><mac address='52:54:00:6f:98:db'/><source network='default'/></interface>
  </devices>
</domain>";

    #[test]
    fn name_validation() {
        assert!(validate_name("vm-01_test.2").is_ok());
        for bad in ["", ".hidden", "a/b", "../x", "a b", "é", &"a".repeat(65)] {
            assert!(validate_name(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn reads_volume_format_and_network_forward_mode() {
        let vol = "<volume><target><format type='qcow2'/></target></volume>";
        assert_eq!(volume_format(vol).unwrap(), "qcow2");
        assert_eq!(volume_format("<volume/>").unwrap(), "unknown");

        let nat = "<network><forward mode='nat'/></network>";
        assert_eq!(network_forward_mode(nat).unwrap().as_deref(), Some("nat"));
        assert_eq!(
            network_forward_mode("<network><forward/></network>")
                .unwrap()
                .as_deref(),
            Some("nat")
        );
        assert_eq!(network_forward_mode("<network/>").unwrap(), None);
    }

    fn spec() -> DomainSpec {
        DomainSpec {
            name: "web-01".into(),
            memory_mb: 2048,
            vcpus: 2,
            kvm: true,
            uefi: false,
            disks: vec![
                DiskAttachment {
                    path: "/mnt/vm/web-01.qcow2".into(),
                    format: "qcow2".into(),
                },
                DiskAttachment {
                    path: "/mnt/vm/data.img".into(),
                    format: "raw".into(),
                },
            ],
            iso_path: Some("/iso/debian.iso".into()),
            network: "default".into(),
            vnc_listen: "0.0.0.0".into(),
        }
    }

    #[test]
    fn new_domain_has_the_expected_devices() {
        let xml = new_domain_xml(&spec()).unwrap();
        let root = Element::parse(xml.as_bytes()).unwrap();
        assert_eq!(root.attributes["type"], "kvm");
        assert_eq!(
            root.get_child("memory").unwrap().get_text().unwrap(),
            "2048"
        );
        assert_eq!(root.get_child("vcpu").unwrap().get_text().unwrap(), "2");
        assert!(
            !root
                .get_child("os")
                .unwrap()
                .attributes
                .contains_key("firmware")
        );

        let devices = root.get_child("devices").unwrap();
        let disks: Vec<_> = devices
            .children
            .iter()
            .filter_map(|n| n.as_element())
            .filter(|e| e.name == "disk")
            .collect();
        assert_eq!(disks.len(), 3, "two disks and a cdrom");
        assert_eq!(
            disks[0].get_child("target").unwrap().attributes["dev"],
            "vda"
        );
        assert_eq!(
            disks[0].get_child("driver").unwrap().attributes["type"],
            "qcow2"
        );
        assert_eq!(
            disks[1].get_child("target").unwrap().attributes["dev"],
            "vdb"
        );
        assert_eq!(
            disks[1].get_child("driver").unwrap().attributes["type"],
            "raw"
        );
        assert_eq!(disks[2].attributes["device"], "cdrom");
        assert!(disks[2].get_child("readonly").is_some());

        let vnc = devices.get_child("graphics").unwrap();
        assert_eq!(vnc.attributes["type"], "vnc");
        assert_eq!(vnc.attributes["listen"], "0.0.0.0");
        assert_eq!(
            devices
                .get_child("interface")
                .unwrap()
                .get_child("source")
                .unwrap()
                .attributes["network"],
            "default"
        );

        // cdrom first, then the disk
        let boot: Vec<_> = root
            .get_child("os")
            .unwrap()
            .children
            .iter()
            .filter_map(|n| n.as_element())
            .filter(|e| e.name == "boot")
            .map(|e| e.attributes["dev"].clone())
            .collect();
        assert_eq!(boot, ["cdrom", "hd"]);
    }

    #[test]
    fn new_domain_uefi_without_iso_boots_the_disk() {
        let mut spec = spec();
        spec.uefi = true;
        spec.iso_path = None;
        let root = Element::parse(new_domain_xml(&spec).unwrap().as_bytes()).unwrap();
        let os = root.get_child("os").unwrap();
        assert_eq!(os.attributes["firmware"], "efi");
        let boot: Vec<_> = os
            .children
            .iter()
            .filter_map(|n| n.as_element())
            .filter(|e| e.name == "boot")
            .collect();
        assert_eq!(boot.len(), 1);
        assert_eq!(boot[0].attributes["dev"], "hd");
    }

    #[test]
    fn new_domain_escapes_hostile_values() {
        let mut spec = spec();
        spec.disks[0].path = "/mnt/it's a \"disk\" & <more>.qcow2".into();
        spec.network = "net'/><evil/>".into();
        let xml = new_domain_xml(&spec).unwrap();
        let root = Element::parse(xml.as_bytes()).expect("still well-formed");
        let devices = root.get_child("devices").unwrap();
        let first = devices.get_child("disk").unwrap();
        assert_eq!(
            first.get_child("source").unwrap().attributes["file"],
            "/mnt/it's a \"disk\" & <more>.qcow2"
        );
        assert!(devices.get_child("evil").is_none(), "no injected element");
        assert!(root.get_child("evil").is_none());
    }

    #[test]
    fn new_domain_on_a_test_driver_is_minimal() {
        let mut spec = spec();
        spec.kvm = false;
        let root = Element::parse(new_domain_xml(&spec).unwrap().as_bytes()).unwrap();
        assert_eq!(root.attributes["type"], "test");
        assert!(root.get_child("cpu").is_none());
        assert!(
            root.get_child("os")
                .unwrap()
                .get_child("type")
                .unwrap()
                .attributes
                .is_empty()
        );
    }

    #[test]
    fn detects_nvram() {
        assert!(has_nvram(XML).unwrap());
        assert!(!has_nvram("<domain><os><type>hvm</type></os></domain>").unwrap());
        assert!(!has_nvram("<domain/>").unwrap());
    }

    #[test]
    fn lists_only_file_disks() {
        assert_eq!(
            disk_sources(XML).unwrap(),
            vec!["/mnt/vm/a.qcow2".to_string()]
        );
    }

    #[test]
    fn clone_rewrites_identity_and_disks() {
        let disks = HashMap::from([(
            "/mnt/vm/a.qcow2".to_string(),
            "/mnt/vm/clone.qcow2".to_string(),
        )]);
        let out = clone_definition(XML, "clone & <co>", &disks).unwrap();
        assert!(!out.contains("<uuid>"));
        assert!(!out.contains("52:54:00:6f:98:db"));
        assert!(!out.contains("source_VARS.fd"));
        assert!(out.contains("template=\"/t.fd\"") || out.contains("template='/t.fd'"));
        assert!(out.contains("/mnt/vm/clone.qcow2"));
        assert!(!out.contains("/mnt/vm/a.qcow2"));
        // the cdrom is shared, untouched
        assert!(out.contains("/iso/install.iso"));
        // the name is escaped, not injected as markup
        assert!(out.contains("clone &amp; &lt;co>") || out.contains("clone &amp; &lt;co&gt;"));
        assert!(!out.contains("<co>"));
    }

    #[test]
    fn volume_copy_keeps_format_and_drops_identity() {
        let vol = "<volume type='file'><name>a.qcow2</name><key>/mnt/vm/a.qcow2</key>
  <capacity unit='bytes'>53687091200</capacity><allocation unit='bytes'>1</allocation>
  <target><path>/mnt/vm/a.qcow2</path><format type='qcow2'/><timestamps><atime>1</atime></timestamps></target>
  <backingStore><path>/base.qcow2</path></backingStore></volume>";
        let out = new_volume_xml(vol, "b.qcow2").unwrap();
        assert!(out.contains("b.qcow2"));
        assert!(out.contains("53687091200"));
        assert!(out.contains("qcow2"));
        for gone in [
            "<key>",
            "/mnt/vm/a.qcow2",
            "<allocation",
            "backingStore",
            "timestamps",
        ] {
            assert!(!out.contains(gone), "{gone} should be removed");
        }
    }

    #[test]
    fn clone_fails_if_a_disk_has_no_copy() {
        assert!(clone_definition(XML, "clone", &HashMap::new()).is_err());
    }
}
