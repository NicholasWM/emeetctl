//! Acha os nós da câmera pelo sysfs (VID:PID), sem depender de número fixo de hidraw/video.

use crate::{Error, Result};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

pub const VID: u16 = 0x328F;
pub const PID: u16 = 0x00EF;

#[derive(Debug, Clone)]
pub struct DevicePaths {
    pub hidraw: Option<PathBuf>,
    pub video: Option<PathBuf>,
    pub usb: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub product: String,
    pub serial: String,
    pub firmware: String,
    pub hidraw: Option<String>,
    pub video: Option<String>,
    /// Outros programas com a câmera aberta agora (ex.: Google Chrome no Meet).
    pub in_use_by: Vec<crate::CameraUser>,
}

/// Procura a S600L. Falha só se nenhum dos dois nós (hidraw, video) existir.
pub fn find() -> Result<DevicePaths> {
    let hidraw = find_hidraw();
    let (video, usb_v) = find_video();
    let usb = usb_v.or_else(|| hidraw.as_ref().and_then(|_| hid_usb_dir()));
    if hidraw.is_none() && video.is_none() {
        return Err(Error::NotFound("nenhum hidraw ou video com 328f:00ef"));
    }
    Ok(DevicePaths { hidraw, video, usb })
}

impl DevicePaths {
    pub fn info(&self) -> DeviceInfo {
        let read = |name: &str| {
            self.usb
                .as_ref()
                .and_then(|d| fs::read_to_string(d.join(name)).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        };
        // bcdDevice 1019 -> firmware "1.0.19", como o EMEET Studio mostra.
        let bcd = read("bcdDevice");
        let firmware = match bcd.len() {
            4 => format!("{}.{}.{}", &bcd[..1], &bcd[1..2], &bcd[2..]),
            _ => bcd.clone(),
        };
        DeviceInfo {
            product: read("product"),
            serial: read("serial"),
            firmware: if bcd.is_empty() {
                String::new()
            } else {
                firmware
            },
            hidraw: self.hidraw.as_ref().map(|p| p.display().to_string()),
            video: self.video.as_ref().map(|p| p.display().to_string()),
            in_use_by: self
                .video
                .as_deref()
                .map(crate::camera_users)
                .unwrap_or_default(),
        }
    }
}

fn find_hidraw() -> Option<PathBuf> {
    let want = format!("HID_ID=0003:{:08X}:{:08X}", VID, PID);
    let mut entries: Vec<_> = fs::read_dir("/sys/class/hidraw").ok()?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    entries.into_iter().find_map(|e| {
        let uevent = fs::read_to_string(e.path().join("device/uevent")).ok()?;
        uevent
            .lines()
            .any(|l| l == want)
            .then(|| Path::new("/dev").join(e.file_name()))
    })
}

fn hid_usb_dir() -> Option<PathBuf> {
    let dev = find_hidraw()?;
    let name = dev.file_name()?;
    usb_parent(&Path::new("/sys/class/hidraw").join(name).join("device"))
}

/// Nó de captura (index 0) da câmera, e o diretório USB dela.
fn find_video() -> (Option<PathBuf>, Option<PathBuf>) {
    let Ok(rd) = fs::read_dir("/sys/class/video4linux") else {
        return (None, None);
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let index = fs::read_to_string(e.path().join("index")).unwrap_or_default();
        if index.trim() != "0" {
            continue;
        }
        if let Some(usb) = usb_parent(&e.path().join("device")) {
            if read_hex(&usb.join("idVendor")) == Some(VID)
                && read_hex(&usb.join("idProduct")) == Some(PID)
            {
                return (Some(Path::new("/dev").join(e.file_name())), Some(usb));
            }
        }
    }
    (None, None)
}

pub(crate) fn usb_parent(start: &Path) -> Option<PathBuf> {
    let mut p = fs::canonicalize(start).ok()?;
    loop {
        if p.join("idVendor").exists() {
            return Some(p);
        }
        if !p.pop() {
            return None;
        }
    }
}

fn read_hex(p: &Path) -> Option<u16> {
    u16::from_str_radix(fs::read_to_string(p).ok()?.trim(), 16).ok()
}
