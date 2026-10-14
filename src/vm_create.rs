//! Validation of a VM creation request, done before touching libvirt.

use std::collections::HashSet;

use crate::{
    errors::ApiError,
    models::CreateVm,
    vm_options::{
        FIRMWARES, MAX_DISK_GB, MAX_MEMORY_MB, MAX_VCPUS, MIN_DISK_GB, MIN_MEMORY_MB, MIN_VCPUS,
    },
    vm_xml,
};

pub const MAX_DISKS: usize = 8;

/// Address the VNC displays of new VMs listen on. The noVNC proxy runs next to the API,
/// not on the hypervisor, so the default has to be reachable from another machine:
/// restrict it with `VNC_LISTEN` (the hypervisor's LAN address) and/or a firewall.
pub fn vnc_listen() -> Result<String, ApiError> {
    parse_vnc_listen(std::env::var("VNC_LISTEN").ok())
}

fn parse_vnc_listen(value: Option<String>) -> Result<String, ApiError> {
    let value = value.unwrap_or_else(|| "0.0.0.0".into());
    match value.parse::<std::net::IpAddr>() {
        Ok(_) => Ok(value),
        Err(_) => {
            eprintln!("VNC_LISTEN is not an IP address: {value:?}");
            Err(ApiError::Internal(
                "Invalid VNC_LISTEN configuration".into(),
            ))
        }
    }
}

pub fn validate(request: &CreateVm) -> Result<(), ApiError> {
    vm_xml::validate_name(&request.name)?;
    in_range("memory_mb", request.memory_mb, MIN_MEMORY_MB, MAX_MEMORY_MB)?;
    in_range("vcpus", request.vcpus, MIN_VCPUS, MAX_VCPUS)?;

    if request.disks.is_empty() && request.iso.is_none() {
        return Err(bad("Give at least one disk or an ISO"));
    }
    if request.disks.len() > MAX_DISKS {
        return Err(bad(&format!("At most {MAX_DISKS} disks per VM")));
    }

    let mut attached = HashSet::new();
    for disk in &request.disks {
        label("disk pool", &disk.pool)?;
        match (disk.size_gb, &disk.volume) {
            (Some(size), None) => in_range("size_gb", size, MIN_DISK_GB, MAX_DISK_GB)?,
            (None, Some(volume)) => {
                label("disk volume", volume)?;
                if !attached.insert((disk.pool.as_str(), volume.as_str())) {
                    return Err(bad("The same volume is attached twice"));
                }
            }
            _ => {
                return Err(bad(
                    "A disk needs exactly one of size_gb (new) or volume (existing)",
                ));
            }
        }
    }
    if let Some(iso) = &request.iso {
        label("iso pool", &iso.pool)?;
        label("iso volume", &iso.volume)?;
    }
    if let Some(network) = &request.network {
        label("network", network)?;
    }
    if let Some(firmware) = &request.firmware
        && !FIRMWARES.contains(&firmware.as_str())
    {
        return Err(bad(&format!("firmware must be one of {FIRMWARES:?}")));
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

/// Names of pools, volumes and networks are looked up in libvirt: keep them printable
/// and free of path separators (a NUL byte would also make the C call panic).
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn request(extra: serde_json::Value) -> CreateVm {
        let mut base = json!({
            "name": "web-01", "memory_mb": 2048, "vcpus": 2,
            "disks": [{ "pool": "datastore", "size_gb": 20 }]
        });
        base.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(base).unwrap()
    }

    fn rejected(extra: serde_json::Value) {
        assert!(
            matches!(
                validate(&request(extra.clone())),
                Err(ApiError::BadRequest(_))
            ),
            "{extra}"
        );
    }

    #[test]
    fn a_minimal_request_is_valid() {
        assert!(validate(&request(json!({}))).is_ok());
    }

    #[test]
    fn a_full_request_is_valid() {
        let full = json!({
            "disks": [
                { "pool": "datastore", "size_gb": 20 },
                { "pool": "datastore", "volume": "data.qcow2" }
            ],
            "iso": { "pool": "Téléchargements", "volume": "debian.iso" },
            "network": "default", "firmware": "uefi", "start": true
        });
        assert!(validate(&request(full)).is_ok());
    }

    #[test]
    fn an_iso_alone_is_enough() {
        let live = json!({ "disks": [], "iso": { "pool": "p", "volume": "v.iso" } });
        assert!(validate(&request(live)).is_ok());
    }

    #[test]
    fn rejects_out_of_range_values() {
        rejected(json!({ "memory_mb": 63 }));
        rejected(json!({ "memory_mb": MAX_MEMORY_MB + 1 }));
        rejected(json!({ "vcpus": 0 }));
        rejected(json!({ "vcpus": MAX_VCPUS + 1 }));
        rejected(json!({ "disks": [{ "pool": "p", "size_gb": 0 }] }));
        rejected(json!({ "disks": [{ "pool": "p", "size_gb": MAX_DISK_GB + 1 }] }));
    }

    #[test]
    fn rejects_bad_names_and_labels() {
        rejected(json!({ "name": "../evil" }));
        rejected(json!({ "disks": [{ "pool": "", "size_gb": 1 }] }));
        rejected(json!({ "disks": [{ "pool": "a\0b", "size_gb": 1 }] }));
        rejected(json!({ "disks": [{ "pool": "p", "volume": "../../etc/passwd" }] }));
        rejected(json!({ "iso": { "pool": "p", "volume": "bad\nname.iso" } }));
        rejected(json!({ "network": "" }));
    }

    #[test]
    fn rejects_inconsistent_disks() {
        rejected(json!({ "disks": [] }));
        rejected(json!({ "disks": [{ "pool": "p" }] }));
        rejected(json!({ "disks": [{ "pool": "p", "size_gb": 1, "volume": "v" }] }));
        let twice =
            json!({ "disks": [{ "pool": "p", "volume": "v" }, { "pool": "p", "volume": "v" }] });
        rejected(twice);
        let many: Vec<_> = (0..=MAX_DISKS)
            .map(|_| json!({ "pool": "p", "size_gb": 1 }))
            .collect();
        rejected(json!({ "disks": many }));
    }

    #[test]
    fn vnc_listen_defaults_to_all_interfaces_and_must_be_an_ip() {
        assert_eq!(parse_vnc_listen(None).unwrap(), "0.0.0.0");
        assert_eq!(
            parse_vnc_listen(Some("192.168.1.10".into())).unwrap(),
            "192.168.1.10"
        );
        assert_eq!(parse_vnc_listen(Some("::1".into())).unwrap(), "::1");
        for bad in ["dbtk", "", "0.0.0.0'/><evil/>", "1.2.3"] {
            assert!(parse_vnc_listen(Some(bad.into())).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn rejects_an_unknown_firmware() {
        rejected(json!({ "firmware": "coreboot" }));
    }
}
