//! Descoberta: placas de som USB com entrada de captura, via sysfs e /proc/asound.

use crate::device::usb_parent;
use crate::{Error, Result};
use serde::Serialize;
use std::fs;
use std::path::Path;

/// Modelos conhecidos: mudam o apelido de seleção e o que a interface explica.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Model {
    EmeetS600l,
    YetiNano,
    Other,
}

impl Model {
    pub fn from_usb(vid: u16, pid: u16) -> Model {
        match (vid, pid) {
            (crate::device::VID, crate::device::PID) => Model::EmeetS600l,
            (0xb58e, 0x0005) => Model::YetiNano,
            _ => Model::Other,
        }
    }

    /// Apelido curto aceito em `--device`.
    pub fn alias(self) -> Option<&'static str> {
        match self {
            Model::EmeetS600l => Some("s600l"),
            Model::YetiNano => Some("nano"),
            Model::Other => None,
        }
    }

    /// Botão de mudo no próprio aparelho que não aparece no mixer: com ele
    /// apertado o microfone manda zeros absolutos.
    pub fn mute_button(self) -> Option<&'static str> {
        match self {
            Model::YetiNano => Some("toque no botão da frente do Yeti Nano (luz vermelha = mudo)"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MicDevice {
    /// Número da placa ALSA (`hw:N`).
    pub card: u32,
    /// Id ALSA da placa (ex.: "Nano").
    pub id: String,
    /// Nome do produto USB (ex.: "Yeti Nano").
    pub name: String,
    pub manufacturer: String,
    /// "b58e:0005".
    pub usb_id: String,
    pub model: Model,
}

impl MicDevice {
    /// Nome para mostrar: fabricante + produto, sem repetir o fabricante.
    pub fn label(&self) -> String {
        if self.manufacturer.is_empty() || self.name.starts_with(&self.manufacturer) {
            self.name.clone()
        } else {
            format!("{} {}", self.manufacturer, self.name)
        }
    }

    /// Nome curto para o seletor da interface.
    pub fn short(&self) -> String {
        match self.model {
            Model::EmeetS600l => "S600L (câmera)".into(),
            Model::YetiNano => "Yeti Nano".into(),
            Model::Other => self.name.clone(),
        }
    }

    /// Casa a seleção do usuário: número da placa, id ALSA, apelido do modelo,
    /// "vid:pid" ou um trecho do nome (sem diferenciar maiúsculas).
    pub fn matches(&self, sel: &str) -> bool {
        let s = sel.trim().to_lowercase();
        s == self.card.to_string()
            || s == self.id.to_lowercase()
            || Some(s.as_str()) == self.model.alias()
            || s == self.usb_id
            || self.label().to_lowercase().contains(&s)
    }
}

fn read(p: &Path) -> String {
    fs::read_to_string(p)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Microfones USB presentes, por número de placa.
pub fn list() -> Vec<MicDevice> {
    let Ok(entries) = fs::read_dir("/sys/class/sound") else {
        return Vec::new();
    };
    let mut mics: Vec<MicDevice> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let card: u32 = name.strip_prefix("card")?.parse().ok()?;
            // Só quem tem captura.
            if !Path::new(&format!("/proc/asound/card{card}/pcm0c")).exists() {
                return None;
            }
            let usb = usb_parent(&e.path().join("device"))?;
            let vid = u16::from_str_radix(&read(&usb.join("idVendor")), 16).ok()?;
            let pid = u16::from_str_radix(&read(&usb.join("idProduct")), 16).ok()?;
            Some(MicDevice {
                card,
                id: read(&e.path().join("id")),
                name: read(&usb.join("product")),
                manufacturer: read(&usb.join("manufacturer")),
                usb_id: format!("{vid:04x}:{pid:04x}"),
                model: Model::from_usb(vid, pid),
            })
        })
        .collect();
    mics.sort_by_key(|m| m.card);
    mics
}

/// Escolhe um microfone. Sem seleção: a entrada padrão do sistema, se for um
/// microfone USB; senão o da câmera; senão o primeiro.
pub fn find(sel: Option<&str>) -> Result<MicDevice> {
    let mics = list();
    match sel {
        Some(s) => mics.into_iter().find(|m| m.matches(s)).ok_or_else(|| {
            Error::Invalid(format!(
                "microfone '{s}' não encontrado (veja `emeetctl mic list`)"
            ))
        }),
        None => {
            let default_card = super::PipeWire::read()
                .ok()
                .and_then(|p| p.default_source_card());
            let i = mics
                .iter()
                .position(|m| Some(m.card) == default_card)
                .or_else(|| mics.iter().position(|m| m.model == Model::EmeetS600l))
                .unwrap_or(0);
            mics.into_iter()
                .nth(i)
                .ok_or(Error::NotFound("nenhum microfone USB conectado"))
        }
    }
}

/// O microfone embutido da câmera.
pub fn camera() -> Result<MicDevice> {
    list()
        .into_iter()
        .find(|m| m.model == Model::EmeetS600l)
        .ok_or(Error::NotFound("microfone 328f:00ef"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nano() -> MicDevice {
        MicDevice {
            card: 2,
            id: "Nano".into(),
            name: "Yeti Nano".into(),
            manufacturer: "Blue Microphones".into(),
            usb_id: "b58e:0005".into(),
            model: Model::from_usb(0xb58e, 0x0005),
        }
    }

    #[test]
    fn selection_and_labels() {
        let m = nano();
        for sel in ["2", "nano", "NANO", "b58e:0005", "yeti", "blue"] {
            assert!(m.matches(sel), "{sel}");
        }
        assert!(!m.matches("s600l"));
        assert_eq!(m.label(), "Blue Microphones Yeti Nano");
        assert_eq!(m.short(), "Yeti Nano");
        assert!(m.model.mute_button().is_some());
        assert_eq!(Model::from_usb(0x328f, 0x00ef), Model::EmeetS600l);
    }
}
