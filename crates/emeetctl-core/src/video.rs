//! Controles de imagem pela API padrão do V4L2 (UVC): brilho, foco, exposição, zoom etc.
//! Nada aqui é proprietário: é o mesmo que `v4l2-ctl` faz, sem depender dele.

use crate::{open_rw, Error, Result};
use serde::Serialize;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;

const VIDIOC_G_CTRL: libc::c_ulong = 0xc008_561b;
const VIDIOC_S_CTRL: libc::c_ulong = 0xc008_561c;
const VIDIOC_QUERYCTRL: libc::c_ulong = 0xc044_5624;
const VIDIOC_QUERYMENU: libc::c_ulong = 0xc02c_5625;
const NEXT_CTRL: u32 = 0x8000_0000;

const FLAG_DISABLED: u32 = 0x0001;
const FLAG_READ_ONLY: u32 = 0x0004;
const FLAG_INACTIVE: u32 = 0x0010;

#[repr(C)]
struct QueryCtrl {
    id: u32,
    kind: u32,
    name: [u8; 32],
    minimum: i32,
    maximum: i32,
    step: i32,
    default_value: i32,
    flags: u32,
    reserved: [u32; 2],
}

#[repr(C)]
struct Ctrl {
    id: u32,
    value: i32,
}

#[repr(C, packed)]
struct QueryMenu {
    id: u32,
    index: u32,
    name: [u8; 32],
    reserved: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ControlKind {
    Integer,
    Boolean,
    Menu { items: Vec<(i32, String)> },
}

#[derive(Debug, Clone, Serialize)]
pub struct Control {
    pub id: u32,
    /// Nome do kernel, ex.: "Focus, Automatic Continuous".
    pub name: String,
    /// Chave curta para CLI/MCP, ex.: "focus_automatic_continuous".
    pub key: String,
    pub kind: ControlKind,
    pub min: i32,
    pub max: i32,
    pub step: i32,
    pub default: i32,
    pub value: i32,
    /// Desligado porque um modo automático está ativo (ex.: foco manual com autofoco ligado).
    pub inactive: bool,
    pub read_only: bool,
}

pub struct Video {
    file: File,
}

fn ioctl<T>(file: &File, req: libc::c_ulong, arg: &mut T) -> std::io::Result<()> {
    let r = unsafe { libc::ioctl(file.as_raw_fd(), req as _, arg as *mut T) };
    if r < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|c| *c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

pub fn key_of(name: &str) -> String {
    let mut k = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            k.push(c.to_ascii_lowercase());
        } else if !k.ends_with('_') && !k.is_empty() {
            k.push('_');
        }
    }
    k.trim_end_matches('_').to_string()
}

impl Video {
    pub fn open(path: &Path) -> Result<Video> {
        Ok(Video { file: open_rw(path, false)? })
    }

    pub fn controls(&self) -> Result<Vec<Control>> {
        let mut out = Vec::new();
        let mut id = NEXT_CTRL;
        loop {
            let mut q = QueryCtrl {
                id,
                kind: 0,
                name: [0; 32],
                minimum: 0,
                maximum: 0,
                step: 0,
                default_value: 0,
                flags: 0,
                reserved: [0; 2],
            };
            if ioctl(&self.file, VIDIOC_QUERYCTRL, &mut q).is_err() {
                break;
            }
            id = q.id | NEXT_CTRL;
            if q.flags & FLAG_DISABLED != 0 {
                continue;
            }
            let kind = match q.kind {
                1 => ControlKind::Integer,
                2 => ControlKind::Boolean,
                3 => ControlKind::Menu { items: self.menu_items(q.id, q.minimum, q.maximum) },
                _ => continue, // classes, botões e tipos compostos ficam fora
            };
            let name = cstr(&q.name);
            out.push(Control {
                id: q.id,
                key: key_of(&name),
                name,
                kind,
                min: q.minimum,
                max: q.maximum,
                step: q.step.max(1),
                default: q.default_value,
                value: self.get(q.id).unwrap_or(q.default_value),
                inactive: q.flags & FLAG_INACTIVE != 0,
                read_only: q.flags & FLAG_READ_ONLY != 0,
            });
        }
        Ok(out)
    }

    fn menu_items(&self, id: u32, min: i32, max: i32) -> Vec<(i32, String)> {
        (min..=max)
            .filter_map(|i| {
                let mut m = QueryMenu { id, index: i as u32, name: [0; 32], reserved: 0 };
                ioctl(&self.file, VIDIOC_QUERYMENU, &mut m).ok()?;
                let name = m.name;
                Some((i, cstr(&name)))
            })
            .collect()
    }

    pub fn get(&self, id: u32) -> Result<i32> {
        let mut c = Ctrl { id, value: 0 };
        ioctl(&self.file, VIDIOC_G_CTRL, &mut c)?;
        Ok(c.value)
    }

    pub fn set(&self, id: u32, value: i32) -> Result<()> {
        let mut c = Ctrl { id, value };
        ioctl(&self.file, VIDIOC_S_CTRL, &mut c).map_err(|e| {
            Error::Invalid(format!("não aceitou o valor {value} no controle 0x{id:08x}: {e}"))
        })
    }

    /// Acha um controle pela chave curta, pelo nome do kernel ou pelo id numérico.
    pub fn find(&self, needle: &str) -> Result<Control> {
        let key = key_of(needle);
        let id = needle.strip_prefix("0x").and_then(|h| u32::from_str_radix(h, 16).ok());
        self.controls()?
            .into_iter()
            .find(|c| c.key == key || Some(c.id) == id)
            .ok_or_else(|| Error::Invalid(format!("controle '{needle}' não existe nesta câmera")))
    }

    /// Volta tudo ao padrão: primeiro os modos automáticos (que destravam os manuais), depois o resto.
    pub fn reset_defaults(&self) -> Result<usize> {
        let ctrls = self.controls()?;
        let mut n = 0;
        let (autos, rest): (Vec<_>, Vec<_>) = ctrls
            .into_iter()
            .filter(|c| !c.read_only)
            .partition(|c| c.kind != ControlKind::Integer);
        for c in autos.iter().chain(rest.iter()) {
            if self.set(c.id, c.default).is_ok() {
                n += 1;
            }
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_match_kernel_abi() {
        assert_eq!(std::mem::size_of::<QueryCtrl>(), 68);
        assert_eq!(std::mem::size_of::<Ctrl>(), 8);
        assert_eq!(std::mem::size_of::<QueryMenu>(), 44);
    }

    #[test]
    fn keys() {
        assert_eq!(key_of("Focus, Automatic Continuous"), "focus_automatic_continuous");
        assert_eq!(key_of("White Balance Temperature"), "white_balance_temperature");
    }
}
