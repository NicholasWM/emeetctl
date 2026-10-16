//! Câmera virtual "emeetctl Camera": lê a S600L uma vez e republica o mesmo
//! quadro em N dispositivos v4l2loopback ("emeetctl Camera", "emeetctl Camera 2",
//! ...), para vários programas (Meet, Zoom, OBS, a prévia do emeetctl) usarem a
//! câmera ao mesmo tempo, cada um na sua saída.
//!
//! - Por que N saídas e não uma: a câmera física só aceita um leitor de stream,
//!   e desde o 0.13 o v4l2loopback também só aceita um leitor de captura por
//!   dispositivo ("only exclusive ownership for each stream" em `vidioc_reqbufs`;
//!   o segundo leitor recebe EBUSY). Copiar o quadro para mais um dispositivo
//!   custa ~1,8 MB de memcpy por quadro.
//! - O dispositivo nasce pelo nó de controle `/dev/v4l2loopback` (ioctl
//!   `V4L2LOOPBACK_CTL_ADD`, ABI do v4l2loopback 0.15), com `announce_all_caps = 0`
//!   ("exclusive caps"): sem isso o Chrome não lista a câmera.
//! - A fonte é a S600L em MJPEG 1280×720 a 30 fps, que é onde o zoom digital
//!   funciona. Cada quadro é decodificado direto em YCbCr e empacotado em YUYV
//!   (4:2:2, faixa limitada), o formato cru que todo app de vídeo aceita.

use crate::{Error, Result};
use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use v4l::buffer::Type;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::video::{Capture, Output};
use v4l::{Device, FourCC};

/// Nome que aparece na lista de câmeras dos apps (da primeira saída).
pub const LABEL: &str = "emeetctl Camera";
/// Saídas criadas por padrão: uma para a chamada, uma para gravar/transmitir e
/// uma para a prévia do próprio emeetctl.
pub const DEFAULT_OUTPUTS: usize = 3;
pub const MAX_OUTPUTS: usize = 8;
pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 720;
pub const FPS: u32 = 30;

const CONTROL: &str = "/dev/v4l2loopback";
// _IOW('~', 1, struct v4l2_loopback_config) com sizeof == 72.
const CTL_ADD: libc::c_ulong = 0x4048_7e01;

/// `struct v4l2_loopback_config` do v4l2loopback 0.15.
#[repr(C)]
struct LoopbackConfig {
    output_nr: i32,
    unused: i32,
    card_label: [u8; 32],
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
    max_buffers: i32,
    max_openers: i32,
    debug: i32,
    announce_all_caps: i32,
}

/// Nome da saída `i` (0 = "emeetctl Camera", 1 = "emeetctl Camera 2", ...).
pub fn label(i: usize) -> String {
    if i == 0 {
        LABEL.to_string()
    } else {
        format!("{LABEL} {}", i + 1)
    }
}

/// Índice da saída a partir do nome do dispositivo, se for uma do emeetctl.
fn output_index(name: &str) -> Option<usize> {
    let rest = name.strip_prefix(LABEL)?;
    if rest.is_empty() {
        return Some(0);
    }
    let n: usize = rest.strip_prefix(' ')?.parse().ok()?;
    (n >= 2).then(|| n - 1)
}

/// Saídas da câmera virtual que já existem, em ordem (primeira = "emeetctl Camera").
pub fn find_devices() -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir("/sys/class/video4linux") else {
        return Vec::new();
    };
    let mut found: Vec<(usize, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = fs::read_to_string(e.path().join("name")).ok()?;
            Some((
                output_index(name.trim())?,
                Path::new("/dev").join(e.file_name()),
            ))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, p)| p).collect()
}

/// O nó da primeira saída, se já existir.
pub fn find_device() -> Option<PathBuf> {
    find_devices().into_iter().next()
}

fn find_output(i: usize) -> Option<PathBuf> {
    let want = label(i);
    fs::read_dir("/sys/class/video4linux")
        .ok()?
        .flatten()
        .find_map(|e| {
            let name = fs::read_to_string(e.path().join("name")).ok()?;
            (name.trim() == want).then(|| Path::new("/dev").join(e.file_name()))
        })
}

/// Cria as `n` saídas que faltarem e devolve todas, em ordem.
pub fn ensure_devices(n: usize) -> Result<Vec<PathBuf>> {
    if n == 0 || n > MAX_OUTPUTS {
        return Err(Error::Invalid(format!("saídas: 1 a {MAX_OUTPUTS}")));
    }
    (0..n)
        .map(|i| match find_output(i) {
            Some(p) => Ok(p),
            None => create_device(&label(i)),
        })
        .collect()
}

fn create_device(name: &str) -> Result<PathBuf> {
    let ctl = OpenOptions::new().read(true).write(true).open(CONTROL).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => Error::Invalid(
            "módulo v4l2loopback não carregado: sudo modprobe v4l2loopback (pacote v4l2loopback-dkms)".into(),
        ),
        std::io::ErrorKind::PermissionDenied => Error::Permission(CONTROL.into()),
        _ => Error::Io(e),
    })?;
    let mut label = [0u8; 32];
    label[..name.len()].copy_from_slice(name.as_bytes());
    let mut cfg = LoopbackConfig {
        output_nr: -1,
        unused: -1,
        card_label: label,
        min_width: 0,
        max_width: 3840,
        min_height: 0,
        max_height: 2160,
        max_buffers: 4,
        max_openers: 16,
        debug: 0,
        announce_all_caps: 0,
    };
    let nr = unsafe {
        libc::ioctl(
            ctl.as_raw_fd(),
            CTL_ADD as _,
            &mut cfg as *mut LoopbackConfig,
        )
    };
    if nr < 0 {
        return Err(Error::Io(std::io::Error::last_os_error()));
    }
    let path = PathBuf::from(format!("/dev/video{nr}"));
    // O udev aplica a permissão do nó novo logo depois de criado.
    for _ in 0..50 {
        if OpenOptions::new().write(true).open(&path).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(path)
}

/// Copia a câmera física para as saídas `sinks` até `stop` virar `true`.
/// `on_fps` recebe a taxa medida a cada ~1 s (para log ou interface).
pub fn run(
    source: &Path,
    sinks: &[PathBuf],
    stop: &AtomicBool,
    mut on_fps: impl FnMut(f32),
) -> Result<()> {
    let src = Device::with_path(source)?;
    let mut fmt = Capture::format(&src)?;
    fmt.width = WIDTH;
    fmt.height = HEIGHT;
    fmt.fourcc = FourCC::new(b"MJPG");
    let fmt = Capture::set_format(&src, &fmt)?;
    if &fmt.fourcc.repr != b"MJPG" || fmt.width != WIDTH || fmt.height != HEIGHT {
        return Err(Error::Invalid(format!(
            "a câmera não aceitou MJPEG {WIDTH}×{HEIGHT} (deu {fmt})"
        )));
    }
    let _ = Capture::set_params(&src, &v4l::video::capture::Parameters::with_fps(FPS));

    let sinks = sinks
        .iter()
        .map(|p| open_sink(p))
        .collect::<Result<Vec<_>>>()?;
    let mut stream = Stream::with_buffers(&src, Type::VideoCapture, 4)?;
    let mut yuyv = vec![0u8; (WIDTH * HEIGHT * 2) as usize];
    let (mut frames, mut since) = (0u32, Instant::now());
    while !stop.load(Ordering::Relaxed) {
        let (buf, meta) = stream.next()?;
        let jpeg = &buf[..(meta.bytesused as usize).min(buf.len())];
        if !mjpeg_to_yuyv(jpeg, WIDTH as usize, HEIGHT as usize, &mut yuyv) {
            continue; // quadro corrompido: pula, não derruba a câmera dos outros
        }
        for sink in &sinks {
            let n = unsafe { libc::write(sink.handle().fd(), yuyv.as_ptr().cast(), yuyv.len()) };
            if n < 0 {
                return Err(Error::Io(std::io::Error::last_os_error()));
            }
        }
        frames += 1;
        if since.elapsed() >= Duration::from_secs(1) {
            on_fps(frames as f32 / since.elapsed().as_secs_f32());
            (frames, since) = (0, Instant::now());
        }
    }
    Ok(())
}

/// Abre uma saída e fixa o formato YUYV 1280×720.
fn open_sink(path: &Path) -> Result<Device> {
    let sink = Device::with_path(path)?;
    let mut out = Output::format(&sink)?;
    out.width = WIDTH;
    out.height = HEIGHT;
    out.fourcc = FourCC::new(b"YUYV");
    out.stride = WIDTH * 2;
    out.size = WIDTH * HEIGHT * 2;
    out.field_order = v4l::format::FieldOrder::Progressive;
    out.colorspace = v4l::format::Colorspace::SRGB;
    Output::set_format(&sink, &out)?;
    Ok(sink)
}

/// Decodifica o JPEG em YCbCr (faixa cheia) e empacota em YUYV de faixa limitada.
fn mjpeg_to_yuyv(jpeg: &[u8], w: usize, h: usize, out: &mut [u8]) -> bool {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::YCbCr);
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(jpeg, opts);
    let Ok(ycc) = dec.decode() else { return false };
    if dec.dimensions() != Some((w, h)) || ycc.len() != w * h * 3 {
        return false;
    }
    pack_yuyv(&ycc, out);
    true
}

/// YCbCr 4:4:4 cheio (JPEG) -> YUYV 4:2:2 limitado (BT.601 de vídeo).
fn pack_yuyv(ycc: &[u8], out: &mut [u8]) {
    #[inline]
    fn luma(y: u8) -> u8 {
        (16 + (y as u32 * 219 + 127) / 255) as u8
    }
    #[inline]
    fn chroma(a: u8, b: u8) -> u8 {
        let c = (a as u32 + b as u32) / 2; // média dos dois pixels
        (16 + (c * 224 + 127) / 255) as u8
    }
    for (px, dst) in ycc.chunks_exact(6).zip(out.chunks_exact_mut(4)) {
        dst[0] = luma(px[0]);
        dst[1] = chroma(px[1], px[4]);
        dst[2] = luma(px[3]);
        dst[3] = chroma(px[2], px[5]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::unnecessary_cast)] // c_ulong é u64 aqui, mas u32 em alvos de 32 bits
    fn config_matches_kernel_abi() {
        assert_eq!(std::mem::size_of::<LoopbackConfig>(), 72);
        // _IOW('~', 1, 72 bytes)
        let expected = (1u64 << 30) | (72 << 16) | (('~' as u64) << 8) | 1;
        assert_eq!(CTL_ADD as u64, expected);
    }

    #[test]
    fn output_labels_round_trip() {
        assert_eq!(label(0), "emeetctl Camera");
        assert_eq!(label(2), "emeetctl Camera 3");
        for i in 0..MAX_OUTPUTS {
            assert_eq!(output_index(&label(i)), Some(i));
        }
        assert_eq!(output_index("emeetctl Camera 1"), None);
        assert_eq!(output_index("emeetctl Cameras"), None);
        assert_eq!(output_index("Iriun Webcam"), None);
    }

    #[test]
    fn packs_full_range_into_limited_yuyv() {
        // dois pixels: preto puro e branco puro, sem cor (Cb = Cr = 128)
        let ycc = [0, 128, 128, 255, 128, 128];
        let mut out = [0u8; 4];
        pack_yuyv(&ycc, &mut out);
        assert_eq!(out, [16, 128, 235, 128]);
    }
}
