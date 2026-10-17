//! Microfone embutido da S600L: no Linux ele é uma placa de som USB à parte
//! (Audio Class), com ganho e mudo no mixer ALSA. O PipeWire diz se ele é a
//! entrada padrão e quais programas estão gravando dele.
//!
//! Só usa os controles padrão de áudio USB (Feature Unit): nenhum comando do
//! fabricante passa por aqui.

use crate::device::{usb_parent, PID, VID};
use crate::{Error, Result};
use alsa::mixer::{MilliBel, Selem, SelemChannelId, SelemId};
use alsa::Round;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// `node.name` do medidor de nível, para não aparecer como "em uso por".
pub const METER_NODE: &str = "emeetctl-meter";
/// Piso do medidor: abaixo disso é silêncio.
pub const FLOOR_DB: f32 = -60.0;
const WINDOW: usize = 48_000 / 20; // 50 ms a 48 kHz
const CH: SelemChannelId = SelemChannelId::FrontLeft; // == mono

#[derive(Debug, Clone, Serialize)]
pub struct MicStatus {
    /// Placa ALSA (`hw:N`).
    pub card: u32,
    /// Ganho em dB, e a faixa que a câmera declara.
    pub gain_db: f32,
    pub min_db: f32,
    pub max_db: f32,
    /// `false` = mudo.
    pub on: bool,
    /// Entrada padrão do sistema no PipeWire.
    pub default: bool,
    /// Programas gravando do microfone agora.
    pub in_use_by: Vec<String>,
    /// `node.name` da entrada no PipeWire.
    pub pipewire_node: Option<String>,
}

impl MicStatus {
    /// Posição do ganho na faixa, 0–100 %.
    pub fn percent(&self) -> f32 {
        let span = self.max_db - self.min_db;
        if span <= 0.0 {
            return 100.0;
        }
        ((self.gain_db - self.min_db) / span * 100.0).clamp(0.0, 100.0)
    }
}

/// Placa ALSA da S600L, achada pelo VID:PID do pai USB.
pub fn find_card() -> Result<u32> {
    let mut cards: Vec<u32> = fs::read_dir("/sys/class/sound")?
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_prefix("card")?.parse().ok())
        .collect();
    cards.sort();
    cards
        .into_iter()
        .find(|c| {
            let dev = Path::new("/sys/class/sound")
                .join(format!("card{c}"))
                .join("device");
            Path::new(&format!("/proc/asound/card{c}/pcm0c")).exists()
                && usb_parent(&dev).is_some_and(|u| {
                    read_hex(&u.join("idVendor")) == Some(VID)
                        && read_hex(&u.join("idProduct")) == Some(PID)
                })
        })
        .ok_or(Error::NotFound("microfone 328f:00ef"))
}

fn read_hex(p: &Path) -> Option<u16> {
    u16::from_str_radix(fs::read_to_string(p).ok()?.trim(), 16).ok()
}

fn alsa_err(e: alsa::Error) -> Error {
    Error::Invalid(format!("ALSA: {e}"))
}

pub struct Mic {
    card: u32,
    mixer: alsa::mixer::Mixer,
    control: String,
}

impl Mic {
    pub fn open() -> Result<Mic> {
        Self::open_card(find_card()?)
    }

    pub fn open_card(card: u32) -> Result<Mic> {
        let mixer = alsa::mixer::Mixer::new(&format!("hw:{card}"), false).map_err(alsa_err)?;
        let mut control = None;
        for elem in mixer.iter() {
            let Some(s) = Selem::new(elem) else { continue };
            let Ok(name) = s.get_id().get_name().map(str::to_string) else {
                continue;
            };
            // Preferir o controle chamado "Mic"; senão, o primeiro com volume.
            if s.has_capture_volume() && (control.is_none() || name == "Mic") {
                control = Some(name);
            }
        }
        let control = control.ok_or(Error::NotFound("ganho do microfone no mixer"))?;
        Ok(Mic {
            card,
            mixer,
            control,
        })
    }

    fn selem(&self) -> Result<Selem<'_>> {
        self.mixer
            .find_selem(&SelemId::new(&self.control, 0))
            .ok_or_else(|| Error::Invalid(format!("controle '{}' sumiu", self.control)))
    }

    pub fn status(&self) -> Result<MicStatus> {
        let s = self.selem()?;
        let (lo, hi) = s.get_capture_db_range();
        let gain = s.get_capture_vol_db(CH).map_err(alsa_err)?;
        let on = !s.has_capture_switch() || s.get_capture_switch(CH).map_err(alsa_err)? != 0;
        let pw = PipeWire::read().ok();
        let node = pw.as_ref().and_then(|p| p.source_for_card(self.card));
        Ok(MicStatus {
            card: self.card,
            gain_db: db(gain),
            min_db: db(lo),
            max_db: db(hi),
            on,
            default: match (&pw, &node) {
                (Some(p), Some((id, _))) => p.default_source() == Some(*id),
                _ => false,
            },
            in_use_by: match (&pw, &node) {
                (Some(p), Some((id, _))) => p.users(*id),
                _ => Vec::new(),
            },
            pipewire_node: node.map(|(_, name)| name),
        })
    }

    /// Ganho em dB (limitado à faixa); devolve o valor que a câmera aplicou.
    pub fn set_gain(&self, value: f32) -> Result<f32> {
        let s = self.selem()?;
        let (lo, hi) = s.get_capture_db_range();
        let v = ((value * 100.0).round() as i64).clamp(lo.0, hi.0);
        s.set_capture_db(CH, MilliBel(v), Round::Floor)
            .map_err(alsa_err)?;
        Ok(db(s.get_capture_vol_db(CH).map_err(alsa_err)?))
    }

    pub fn set_on(&self, on: bool) -> Result<()> {
        let s = self.selem()?;
        if !s.has_capture_switch() {
            return Err(Error::Invalid("o microfone não tem mudo".into()));
        }
        s.set_capture_switch_all(on as i32).map_err(alsa_err)
    }

    /// Torna o microfone da câmera a entrada padrão do sistema.
    pub fn make_default(&self) -> Result<()> {
        let (id, _) = PipeWire::read()?
            .source_for_card(self.card)
            .ok_or(Error::NotFound("entrada da câmera no PipeWire"))?;
        let st = Command::new("wpctl")
            .args(["set-default", &id.to_string()])
            .status()
            .map_err(|e| Error::Invalid(format!("wpctl: {e}")))?;
        if st.success() {
            Ok(())
        } else {
            Err(Error::Invalid(format!("wpctl set-default {id} falhou")))
        }
    }
}

fn db(m: MilliBel) -> f32 {
    m.0 as f32 / 100.0
}

// ---------- PipeWire ----------

/// Retrato do grafo do PipeWire (`pw-dump`).
pub struct PipeWire(Vec<Value>);

fn prop<'a>(o: &'a Value, k: &str) -> Option<&'a Value> {
    o.pointer("/info/props")?.get(k)
}

fn prop_str<'a>(o: &'a Value, k: &str) -> Option<&'a str> {
    prop(o, k)?.as_str()
}

impl PipeWire {
    pub fn read() -> Result<PipeWire> {
        let out = Command::new("pw-dump")
            .output()
            .map_err(|e| Error::Invalid(format!("pw-dump: {e} (PipeWire instalado?)")))?;
        Self::parse(&out.stdout)
    }

    pub fn parse(json: &[u8]) -> Result<PipeWire> {
        let v: Value =
            serde_json::from_slice(json).map_err(|e| Error::Invalid(format!("pw-dump: {e}")))?;
        Ok(PipeWire(v.as_array().cloned().unwrap_or_default()))
    }

    fn nodes(&self) -> impl Iterator<Item = &Value> {
        self.0
            .iter()
            .filter(|o| o.get("type").and_then(Value::as_str) == Some("PipeWire:Interface:Node"))
    }

    fn node(&self, id: u64) -> Option<&Value> {
        self.nodes()
            .find(|o| o.get("id").and_then(Value::as_u64) == Some(id))
    }

    /// Id e `node.name` da entrada (Audio/Source) da placa `card`.
    pub fn source_for_card(&self, card: u32) -> Option<(u64, String)> {
        self.nodes().find_map(|o| {
            let c = prop(o, "alsa.card")
                .or_else(|| prop(o, "api.alsa.pcm.card"))?
                .as_u64()?;
            if prop_str(o, "media.class")? != "Audio/Source" || c != card as u64 {
                return None;
            }
            Some((
                o.get("id")?.as_u64()?,
                prop_str(o, "node.name")?.to_string(),
            ))
        })
    }

    /// Id do nó da entrada padrão (`default.audio.source`).
    pub fn default_source(&self) -> Option<u64> {
        let name = self
            .0
            .iter()
            .filter(|o| {
                o.get("type").and_then(Value::as_str) == Some("PipeWire:Interface:Metadata")
            })
            .flat_map(|o| {
                o.get("metadata")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .find(|m| m.get("key").and_then(Value::as_str) == Some("default.audio.source"))?
            .pointer("/value/name")?
            .as_str()?
            .to_string();
        self.nodes().find_map(|o| {
            (prop_str(o, "node.name") == Some(name.as_str()))
                .then(|| o.get("id")?.as_u64())
                .flatten()
        })
    }

    /// Programas com stream ligado ao nó `source` (sem repetir, sem o medidor).
    pub fn users(&self, source: u64) -> Vec<String> {
        let mut apps = BTreeSet::new();
        for l in self
            .0
            .iter()
            .filter(|o| o.get("type").and_then(Value::as_str) == Some("PipeWire:Interface:Link"))
        {
            let info = &l["info"];
            if info["output-node-id"].as_u64() != Some(source) {
                continue;
            }
            let Some(n) = info["input-node-id"].as_u64().and_then(|i| self.node(i)) else {
                continue;
            };
            if prop_str(n, "node.name") == Some(METER_NODE) {
                continue;
            }
            let app = prop_str(n, "application.name")
                .or_else(|| prop_str(n, "application.process.binary"))
                .or_else(|| prop_str(n, "node.name"))
                .unwrap_or("?");
            apps.insert(app.to_string());
        }
        apps.into_iter().collect()
    }
}

// ---------- medidor ----------

/// Nível ao vivo: `pw-record` do nó (mono, s16, 48 kHz), pico de cada janela de 50 ms.
pub struct Meter {
    child: Child,
    peak: Arc<AtomicU32>,
}

impl Meter {
    /// `on_level` roda a cada janela (para a GUI pedir repintura).
    pub fn start(node_name: &str, on_level: impl Fn() + Send + 'static) -> Result<Meter> {
        let mut child = Command::new("pw-record")
            .args(["--target", node_name, "--rate", "48000", "--channels", "1"])
            .args(["--format", "s16", "--latency", "20ms", "-P"])
            .arg(format!("{{ node.name = {METER_NODE} }}"))
            .arg("-")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::Invalid(format!("pw-record: {e}")))?;
        let mut out = child.stdout.take().expect("stdout piped");
        let peak = Arc::new(AtomicU32::new(FLOOR_DB.to_bits()));
        let p = peak.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; WINDOW * 2];
            while out.read_exact(&mut buf).is_ok() {
                p.store(peak_dbfs(&buf).to_bits(), Ordering::Relaxed);
                on_level();
            }
        });
        Ok(Meter { child, peak })
    }

    /// Pico da última janela, em dBFS (FLOOR_DB a 0).
    pub fn peak_db(&self) -> f32 {
        f32::from_bits(self.peak.load(Ordering::Relaxed))
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Pico de um bloco s16 little-endian, em dBFS, com piso em FLOOR_DB.
pub fn peak_dbfs(pcm: &[u8]) -> f32 {
    let max = pcm
        .chunks_exact(2)
        .map(|b| (i16::from_le_bytes([b[0], b[1]]) as i32).unsigned_abs())
        .max()
        .unwrap_or(0);
    if max == 0 {
        return FLOOR_DB;
    }
    (20.0 * (max as f32 / 32768.0).log10()).max(FLOOR_DB)
}

/// 0–1 para desenhar a barra, linear em dB a partir do piso.
pub fn fraction(db: f32) -> f32 {
    ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn peak_in_dbfs() {
        assert_eq!(peak_dbfs(&pcm(&[0, 0])), FLOOR_DB);
        assert!(peak_dbfs(&pcm(&[0, i16::MIN])).abs() < 0.01);
        assert!((peak_dbfs(&pcm(&[16384, -3])) + 6.02).abs() < 0.01);
        assert_eq!(fraction(-30.0), 0.5);
    }

    const DUMP: &str = r#"[
      {"id": 0, "type": "PipeWire:Interface:Metadata", "props": {"metadata.name": "default"},
       "metadata": [{"subject": 0, "key": "default.audio.source", "type": "Spa:String:JSON",
                     "value": {"name": "alsa_input.s600l"}}]},
      {"id": 91, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Audio/Source", "alsa.card": 1, "node.name": "alsa_input.s600l"}}},
      {"id": 200, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Stream/Input/Audio", "application.name": "Google Chrome"}}},
      {"id": 201, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Stream/Input/Audio", "node.name": "emeetctl-meter"}}},
      {"id": 300, "type": "PipeWire:Interface:Link", "info": {"output-node-id": 91, "input-node-id": 200}},
      {"id": 301, "type": "PipeWire:Interface:Link", "info": {"output-node-id": 91, "input-node-id": 201}}
    ]"#;

    #[test]
    fn pipewire_card_default_and_users() {
        let p = PipeWire::parse(DUMP.as_bytes()).unwrap();
        assert_eq!(p.source_for_card(1), Some((91, "alsa_input.s600l".into())));
        assert!(p.source_for_card(2).is_none());
        assert_eq!(p.default_source(), Some(91));
        assert_eq!(p.users(91), vec!["Google Chrome".to_string()]);
    }
}
