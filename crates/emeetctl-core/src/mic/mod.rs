//! Microfones USB: o embutido da S600L e qualquer outro (ex.: Yeti Nano). No
//! Linux cada um é uma placa de som USB (Audio Class), com ganho e mudo no
//! mixer ALSA; o PipeWire diz qual é a entrada padrão e quem está gravando.
//!
//! Só usa os controles padrão de áudio USB (Feature Unit): nenhum comando do
//! fabricante passa por aqui.

pub mod clean;
mod device;

pub use device::{camera, find, list, MicDevice, Model};

use crate::{Error, Result};
use alsa::mixer::{MilliBel, Selem, SelemChannelId, SelemId};
use alsa::Round;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// `node.name` do medidor de nível, para não aparecer como "em uso por".
pub const METER_NODE: &str = "emeetctl-meter";
/// Piso do medidor: abaixo disso é silêncio.
pub const FLOOR_DB: f32 = -60.0;
const WINDOW: usize = 48_000 / 20; // 50 ms a 48 kHz
/// Janelas seguidas de zeros absolutos que contam como "silêncio digital" (~1 s).
const ZERO_WINDOWS: u32 = 20;
const CH: SelemChannelId = SelemChannelId::FrontLeft; // == mono

/// Volume e mudo de uma saída de fone no próprio microfone (ex.: Yeti Nano).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Level {
    pub db: f32,
    pub min_db: f32,
    pub max_db: f32,
    /// `false` = mudo.
    pub on: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MicStatus {
    #[serde(flatten)]
    pub device: MicDevice,
    /// Ganho em dB, e a faixa que o microfone declara.
    pub gain_db: f32,
    pub min_db: f32,
    pub max_db: f32,
    /// `false` = mudo.
    pub on: bool,
    /// Saída de fone, se o microfone tiver uma.
    pub headphones: Option<Level>,
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

fn alsa_err(e: alsa::Error) -> Error {
    Error::Invalid(format!("ALSA: {e}"))
}

pub struct Mic {
    device: MicDevice,
    mixer: alsa::mixer::Mixer,
    capture: String,
    playback: Option<String>,
}

impl Mic {
    /// O microfone embutido da câmera.
    pub fn open() -> Result<Mic> {
        Self::open_device(camera()?)
    }

    /// Microfone escolhido por `sel` (placa, apelido "nano"/"s600l", vid:pid ou
    /// trecho do nome); sem seleção, a entrada padrão do sistema.
    pub fn find(sel: Option<&str>) -> Result<Mic> {
        Self::open_device(find(sel)?)
    }

    pub fn open_device(device: MicDevice) -> Result<Mic> {
        let mixer =
            alsa::mixer::Mixer::new(&format!("hw:{}", device.card), false).map_err(alsa_err)?;
        let mut capture = None;
        let mut playback = None;
        for elem in mixer.iter() {
            let Some(s) = Selem::new(elem) else { continue };
            let Ok(name) = s.get_id().get_name().map(str::to_string) else {
                continue;
            };
            // Preferir o controle chamado "Mic"; senão, o primeiro com volume.
            if s.has_capture_volume() && (capture.is_none() || name == "Mic") {
                capture = Some(name.clone());
            }
            if s.has_playback_volume() && playback.is_none() {
                playback = Some(name);
            }
        }
        let capture = capture.ok_or(Error::NotFound("ganho do microfone no mixer"))?;
        Ok(Mic {
            device,
            mixer,
            capture,
            playback,
        })
    }

    pub fn device(&self) -> &MicDevice {
        &self.device
    }

    fn selem(&self, name: &str) -> Result<Selem<'_>> {
        self.mixer
            .find_selem(&SelemId::new(name, 0))
            .ok_or_else(|| Error::Invalid(format!("controle '{name}' sumiu")))
    }

    fn headphones_selem(&self) -> Result<Selem<'_>> {
        let name = self
            .playback
            .as_deref()
            .ok_or_else(|| Error::Invalid("este microfone não tem saída de fone".into()))?;
        self.selem(name)
    }

    pub fn status(&self) -> Result<MicStatus> {
        let s = self.selem(&self.capture)?;
        let (lo, hi) = s.get_capture_db_range();
        let gain = s.get_capture_vol_db(CH).map_err(alsa_err)?;
        let on = !s.has_capture_switch() || s.get_capture_switch(CH).map_err(alsa_err)? != 0;
        let headphones = match self.headphones_selem() {
            Ok(h) => {
                let (lo, hi) = h.get_playback_db_range();
                Some(Level {
                    db: db(h.get_playback_vol_db(CH).map_err(alsa_err)?),
                    min_db: db(lo),
                    max_db: db(hi),
                    on: !h.has_playback_switch()
                        || h.get_playback_switch(CH).map_err(alsa_err)? != 0,
                })
            }
            Err(_) => None,
        };
        let pw = PipeWire::read().ok();
        let node = pw
            .as_ref()
            .and_then(|p| p.source_for_card(self.device.card));
        Ok(MicStatus {
            device: self.device.clone(),
            gain_db: db(gain),
            min_db: db(lo),
            max_db: db(hi),
            on,
            headphones,
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

    /// Ganho em dB (limitado à faixa); devolve o valor que o microfone aplicou.
    pub fn set_gain(&self, value: f32) -> Result<f32> {
        let s = self.selem(&self.capture)?;
        let (lo, hi) = s.get_capture_db_range();
        s.set_capture_db(CH, clamp_mb(value, lo, hi), Round::Floor)
            .map_err(alsa_err)?;
        Ok(db(s.get_capture_vol_db(CH).map_err(alsa_err)?))
    }

    pub fn set_on(&self, on: bool) -> Result<()> {
        let s = self.selem(&self.capture)?;
        if !s.has_capture_switch() {
            return Err(Error::Invalid("o microfone não tem mudo".into()));
        }
        s.set_capture_switch_all(on as i32).map_err(alsa_err)
    }

    /// Volume do fone em dB (limitado à faixa); devolve o valor aplicado.
    pub fn set_headphones(&self, value: f32) -> Result<f32> {
        let s = self.headphones_selem()?;
        let (lo, hi) = s.get_playback_db_range();
        s.set_playback_db_all(clamp_mb(value, lo, hi), Round::Floor)
            .map_err(alsa_err)?;
        Ok(db(s.get_playback_vol_db(CH).map_err(alsa_err)?))
    }

    pub fn set_headphones_on(&self, on: bool) -> Result<()> {
        let s = self.headphones_selem()?;
        if !s.has_playback_switch() {
            return Err(Error::Invalid("o fone não tem mudo".into()));
        }
        s.set_playback_switch_all(on as i32).map_err(alsa_err)
    }

    /// Torna este microfone a entrada padrão do sistema.
    pub fn make_default(&self) -> Result<()> {
        let (id, _) = PipeWire::read()?
            .source_for_card(self.device.card)
            .ok_or(Error::NotFound("entrada do microfone no PipeWire"))?;
        set_default(id)
    }
}

/// Torna o nó `id` a entrada padrão do sistema (`wpctl set-default`).
pub(crate) fn set_default(id: u64) -> Result<()> {
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

fn clamp_mb(value: f32, lo: MilliBel, hi: MilliBel) -> MilliBel {
    MilliBel(((value * 100.0).round() as i64).clamp(lo.0, hi.0))
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

    /// Id do nó com este `node.name`.
    pub fn node_id(&self, name: &str) -> Option<u64> {
        self.nodes().find_map(|o| {
            (prop_str(o, "node.name") == Some(name))
                .then(|| o.get("id")?.as_u64())
                .flatten()
        })
    }

    /// Placa ALSA da entrada padrão, se ela for uma placa ALSA.
    pub fn default_source_card(&self) -> Option<u32> {
        let n = self.node(self.default_source()?)?;
        prop(n, "alsa.card")
            .or_else(|| prop(n, "api.alsa.pcm.card"))?
            .as_u64()
            .map(|c| c as u32)
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
            let name = prop_str(n, "node.name");
            if name == Some(METER_NODE) {
                continue;
            }
            // O microfone limpo lê daqui: quem usa ele, usa este microfone.
            if name == Some(&format!("{}.capture", clean::NODE)) {
                if let Some(id) = self.node_id(clean::NODE) {
                    apps.extend(self.users(id).into_iter().map(|a| format!("{a} (limpo)")));
                }
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
    zeros: Arc<AtomicU32>,
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
        let zeros = Arc::new(AtomicU32::new(0));
        let (p, z) = (peak.clone(), zeros.clone());
        std::thread::spawn(move || {
            let mut buf = vec![0u8; WINDOW * 2];
            while out.read_exact(&mut buf).is_ok() {
                p.store(peak_dbfs(&buf).to_bits(), Ordering::Relaxed);
                if buf.iter().all(|&b| b == 0) {
                    z.fetch_add(1, Ordering::Relaxed);
                } else {
                    z.store(0, Ordering::Relaxed);
                }
                on_level();
            }
        });
        Ok(Meter { child, peak, zeros })
    }

    /// Pico da última janela, em dBFS (FLOOR_DB a 0).
    pub fn peak_db(&self) -> f32 {
        f32::from_bits(self.peak.load(Ordering::Relaxed))
    }
}

impl Meter {
    /// Só zeros absolutos há ~1 s. Microfone de verdade sempre tem algum ruído;
    /// zero exato quase sempre é mudo no próprio aparelho, que o mixer não vê
    /// (ex.: botão do Yeti Nano).
    pub fn digital_silence(&self) -> bool {
        self.zeros.load(Ordering::Relaxed) >= ZERO_WINDOWS
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
      {"id": 210, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Stream/Input/Audio", "node.name": "emeetctl-clean.capture"}}},
      {"id": 211, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Audio/Source", "node.name": "emeetctl-clean"}}},
      {"id": 212, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Stream/Input/Audio", "application.name": "Zoom"}}},
      {"id": 302, "type": "PipeWire:Interface:Link", "info": {"output-node-id": 91, "input-node-id": 210}},
      {"id": 303, "type": "PipeWire:Interface:Link", "info": {"output-node-id": 211, "input-node-id": 212}},
      {"id": 300, "type": "PipeWire:Interface:Link", "info": {"output-node-id": 91, "input-node-id": 200}},
      {"id": 301, "type": "PipeWire:Interface:Link", "info": {"output-node-id": 91, "input-node-id": 201}}
    ]"#;

    #[test]
    fn pipewire_card_default_and_users() {
        let p = PipeWire::parse(DUMP.as_bytes()).unwrap();
        assert_eq!(p.source_for_card(1), Some((91, "alsa_input.s600l".into())));
        assert!(p.source_for_card(2).is_none());
        assert_eq!(p.default_source(), Some(91));
        assert_eq!(p.default_source_card(), Some(1));
        assert_eq!(
            p.users(91),
            vec!["Google Chrome".to_string(), "Zoom (limpo)".to_string()]
        );
        assert_eq!(p.node_id("emeetctl-clean"), Some(211));
    }
}
