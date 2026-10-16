//! Protocolo HID da luz (Fill Light + Ambient RGB).
//!
//! Frame de 32 bytes no hidraw da interface 4 (interrupt OUT ep 0x01, resposta no IN 0x84):
//!
//! ```text
//! 09 02 02 OP LL LL LL LL <dados> 00…   LL = tamanho dos dados (u16 BE, repetido), sem checksum
//! ```
//!
//! | op      | o quê             | dados                                   |
//! |---------|-------------------|-----------------------------------------|
//! | 00 / 01 | liga/desliga, lê  | `ff 0X 00 00` Fill · `03 0X 0X 0X` Ambient |
//! | 02 / 03 | cor, lê           | `00 RR GG BB`                           |
//! | 04 / 05 | brilho, lê        | `MM BB` (o arco-íris usa o MM do Ambient) |
//! | 06 / 07 | modo, lê          | `00 MM`                                 |
//!
//! O ACK de todo SET é um byte `20`. A câmera também empurra frames de estado sozinha
//! quando o modo muda, por isso cada leitura filtra pelo opcode esperado.

use crate::{open_rw, Error, Result};
use serde::Serialize;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

const FRAME: usize = 32;
const HEADER: [u8; 3] = [0x09, 0x02, 0x02];
const ACK: u8 = 0x20;
const TIMEOUT: Duration = Duration::from_millis(500);
/// Opcodes vistos na captura. O canal pode ter comandos de firmware: nada fora daqui sai.
const ALLOWED: [u8; 8] = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Cool = 0,
    Warm = 1,
    Balanced = 2,
    Ambient = 3,
    Rainbow = 4,
}

impl Mode {
    pub const ALL: [Mode; 5] = [Mode::Cool, Mode::Warm, Mode::Balanced, Mode::Ambient, Mode::Rainbow];

    pub fn from_byte(b: u8) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| *m as u8 == b)
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Cool => "cool",
            Mode::Warm => "warm",
            Mode::Balanced => "balanced",
            Mode::Ambient => "ambient",
            Mode::Rainbow => "rainbow",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.name().eq_ignore_ascii_case(s))
    }

    /// Byte de modo usado pelos comandos de brilho: o arco-íris compartilha o do Ambient.
    fn brightness_key(self) -> u8 {
        match self {
            Mode::Rainbow => Mode::Ambient as u8,
            m => m as u8,
        }
    }

    /// Faixa crua do slider no app. Warm/Balanced não foram medidos: assume a do Cool.
    pub fn brightness_range(self) -> (u8, u8) {
        match self {
            Mode::Ambient | Mode::Rainbow => (0x14, 0xFF),
            _ => (0x37, 0x64),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    /// A luz de preenchimento inteira.
    Fill,
    /// O anel RGB.
    Ambient,
}

impl Target {
    fn byte(self) -> u8 {
        match self {
            Target::Fill => 0xFF,
            Target::Ambient => 0x03,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LightStatus {
    pub fill_light: Option<bool>,
    pub ambient: Option<bool>,
    pub mode: Option<Mode>,
    /// Cor gravada do Ambient, `RRGGBB`.
    pub color: Option<String>,
    pub brightness_raw: Option<u8>,
    /// Brilho em % da faixa do modo atual.
    pub brightness_percent: Option<u8>,
}

pub struct Light {
    file: File,
}

impl Light {
    pub fn open(path: &Path) -> Result<Light> {
        Ok(Light { file: open_rw(path, true)? })
    }

    fn wait_readable(&self, timeout: Duration) -> bool {
        let mut pfd = libc::pollfd { fd: self.file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        unsafe { libc::poll(&mut pfd, 1, ms) > 0 }
    }

    fn drain(&mut self) {
        let mut buf = [0u8; 64];
        while self.wait_readable(Duration::ZERO) {
            if self.file.read(&mut buf).is_err() {
                break;
            }
        }
    }

    fn read_reply(&mut self, op: u8, timeout: Duration) -> Result<Option<Vec<u8>>> {
        let end = Instant::now() + timeout;
        let mut buf = [0u8; 64];
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() || !self.wait_readable(left) {
                return Ok(None);
            }
            let n = match self.file.read(&mut buf) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(e) => return Err(e.into()),
            };
            if n >= 8 && buf[..3] == HEADER && buf[3] == op {
                let len = u16::from_be_bytes([buf[4], buf[5]]) as usize;
                return Ok(Some(buf[8..(8 + len).min(n)].to_vec()));
            }
        }
    }

    fn write_frame(&mut self, op: u8, data: &[u8]) -> Result<()> {
        if !ALLOWED.contains(&op) {
            return Err(Error::Invalid(format!("opcode 0x{op:02x} fora da lista observada")));
        }
        let len = (data.len() as u16).to_be_bytes();
        let mut frame = [0u8; FRAME];
        frame[..3].copy_from_slice(&HEADER);
        frame[3] = op;
        frame[4..6].copy_from_slice(&len);
        frame[6..8].copy_from_slice(&len);
        frame[8..8 + data.len()].copy_from_slice(data);
        self.drain();
        self.file.write_all(&frame)?;
        Ok(())
    }

    fn request(&mut self, op: u8, data: &[u8]) -> Result<Vec<u8>> {
        self.write_frame(op, data)?;
        self.read_reply(op, TIMEOUT)?
            .ok_or_else(|| Error::Protocol(format!("sem resposta ao op 0x{op:02x}")))
    }

    fn set(&mut self, op: u8, data: &[u8]) -> Result<()> {
        let r = self.request(op, data)?;
        if r.first() == Some(&ACK) {
            Ok(())
        } else {
            Err(Error::Protocol(format!("op 0x{op:02x} respondeu {r:02x?} em vez de ACK")))
        }
    }

    // ---- leituras ----

    pub fn mode(&mut self) -> Result<Mode> {
        let r = self.request(0x07, &[0x20])?;
        r.get(1)
            .and_then(|b| Mode::from_byte(*b))
            .ok_or_else(|| Error::Protocol(format!("modo desconhecido {r:02x?}")))
    }

    pub fn power_state(&mut self) -> Result<(Option<bool>, Option<bool>)> {
        // O GET de liga/desliga responde dois frames: Fill (ff) e Ambient (03).
        self.write_frame(0x01, &[0x03])?;
        let (mut fill, mut ambient) = (None, None);
        let end = Instant::now() + TIMEOUT;
        while fill.is_none() || ambient.is_none() {
            let left = end.saturating_duration_since(Instant::now());
            match self.read_reply(0x01, left)? {
                Some(r) if r.len() >= 2 && r[0] == 0xFF => fill = Some(r[1] != 0),
                Some(r) if r.len() >= 2 && r[0] == 0x03 => ambient = Some(r[1] != 0),
                Some(_) => {}
                None => break,
            }
        }
        Ok((fill, ambient))
    }

    pub fn color(&mut self) -> Result<[u8; 3]> {
        let r = self.request(0x03, &[0x00])?;
        match r.as_slice() {
            [_, r, g, b, ..] => Ok([*r, *g, *b]),
            _ => Err(Error::Protocol(format!("cor {r:02x?}"))),
        }
    }

    pub fn brightness_raw(&mut self, mode: Mode) -> Result<u8> {
        let r = self.request(0x05, &[mode.brightness_key()])?;
        r.get(1).copied().ok_or_else(|| Error::Protocol(format!("brilho {r:02x?}")))
    }

    pub fn status(&mut self) -> Result<LightStatus> {
        let (fill_light, ambient) = self.power_state()?;
        let mode = self.mode().ok();
        let color = self.color().ok().map(hex);
        let brightness_raw = mode.and_then(|m| self.brightness_raw(m).ok());
        let brightness_percent = mode.zip(brightness_raw).map(|(m, raw)| raw_to_percent(m, raw));
        Ok(LightStatus { fill_light, ambient, mode, color, brightness_raw, brightness_percent })
    }

    // ---- escritas ----

    pub fn set_power(&mut self, target: Target, on: bool) -> Result<()> {
        let data = match (target, on) {
            (Target::Ambient, true) => [0x03, 1, 1, 1],
            (t, on) => [t.byte(), on as u8, 0, 0],
        };
        self.set(0x00, &data)
    }

    pub fn set_mode(&mut self, mode: Mode) -> Result<()> {
        self.set(0x06, &[0x00, mode as u8])
    }

    /// Qualquer RGB; põe o Ambient em cor fixa antes, como o app faz.
    pub fn set_color(&mut self, rgb: [u8; 3]) -> Result<()> {
        self.set_mode(Mode::Ambient)?;
        self.set(0x02, &[0x00, rgb[0], rgb[1], rgb[2]])
    }

    pub fn set_brightness_raw(&mut self, mode: Mode, raw: u8) -> Result<u8> {
        let (lo, hi) = mode.brightness_range();
        let raw = raw.clamp(lo, hi);
        // O app não espera ACK de brilho (manda 04 seguido de 05); aceitamos qualquer resposta.
        self.write_frame(0x04, &[mode.brightness_key(), raw])?;
        let _ = self.read_reply(0x04, TIMEOUT)?;
        Ok(raw)
    }

    pub fn set_brightness_percent(&mut self, percent: u8) -> Result<(Mode, u8)> {
        let mode = self.mode()?;
        let raw = percent_to_raw(mode, percent);
        Ok((mode, self.set_brightness_raw(mode, raw)?))
    }
}

pub fn percent_to_raw(mode: Mode, percent: u8) -> u8 {
    let (lo, hi) = mode.brightness_range();
    let p = percent.min(100) as u32;
    (lo as u32 + (hi as u32 - lo as u32) * p / 100) as u8
}

pub fn raw_to_percent(mode: Mode, raw: u8) -> u8 {
    let (lo, hi) = mode.brightness_range();
    let raw = raw.clamp(lo, hi);
    (((raw - lo) as u32 * 100 + (hi - lo) as u32 / 2) / (hi - lo) as u32) as u8
}

pub fn hex(rgb: [u8; 3]) -> String {
    format!("{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

pub fn parse_hex(s: &str) -> Result<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    let v = (s.len() == 6).then(|| u32::from_str_radix(s, 16).ok()).flatten();
    match v {
        Some(v) => Ok([(v >> 16) as u8, (v >> 8) as u8, v as u8]),
        None => Err(Error::Invalid(format!("cor inválida '{s}': use RRGGBB"))),
    }
}

/// As cores predefinidas do EMEET Studio 2.0.4 (o arco-íris é o modo Rainbow).
pub const APP_PRESETS: [(&str, [u8; 3]); 6] = [
    ("azul", [0x00, 0x00, 0xFF]),
    ("rosa", [0xCD, 0x32, 0x37]),
    ("laranja", [0xFF, 0x3C, 0x00]),
    ("amarelo", [0xFF, 0x95, 0x00]),
    ("ciano", [0x00, 0xFF, 0x95]),
    ("roxo", [0xFF, 0x00, 0xB3]),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_roundtrip_edges() {
        assert_eq!(percent_to_raw(Mode::Ambient, 0), 0x14);
        assert_eq!(percent_to_raw(Mode::Ambient, 100), 0xFF);
        assert_eq!(percent_to_raw(Mode::Cool, 100), 0x64);
        assert_eq!(raw_to_percent(Mode::Ambient, 0xFF), 100);
        assert_eq!(raw_to_percent(Mode::Cool, 0x37), 0);
    }

    #[test]
    fn parses_colors() {
        assert_eq!(parse_hex("#123456").unwrap(), [0x12, 0x34, 0x56]);
        assert!(parse_hex("12345").is_err());
        assert_eq!(hex([0xff, 0, 0xb3]), "ff00b3");
    }

    #[test]
    fn rainbow_uses_ambient_brightness_key() {
        assert_eq!(Mode::Rainbow.brightness_key(), 3);
        assert_eq!(Mode::Warm.brightness_key(), 1);
    }
}
