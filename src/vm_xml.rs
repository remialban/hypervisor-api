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
