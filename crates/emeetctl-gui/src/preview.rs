//! Pré-visualização da câmera: uma thread captura do V4L2 e deixa o quadro mais novo
//! num slot compartilhado. Ligar/desligar a prévia abre/fecha o stream, para não
//! segurar a câmera (e o LED de uso) quando ninguém está olhando.

use eframe::egui::{self, ColorImage};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use v4l::buffer::Type;
use v4l::io::mmap::Stream;
use v4l::io::traits::CaptureStream;
use v4l::video::Capture;
use v4l::{Device, FourCC};

const WIDTH: u32 = 640;
const HEIGHT: u32 = 480;

#[derive(Default)]
pub struct Shared {
    pub frame: Option<ColorImage>,
    pub error: Option<String>,
    pub format: Option<String>,
}

pub struct Preview {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    pub shared: Arc<Mutex<Shared>>,
}

impl Preview {
    pub fn start(path: PathBuf, ctx: egui::Context) -> Preview {
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (s, sh) = (stop.clone(), shared.clone());
        let handle = std::thread::spawn(move || {
            if let Err(e) = capture(&path, &s, &sh, &ctx) {
                let msg = if e.raw_os_error() == Some(EBUSY) {
                    "a câmera está em uso por outro programa".to_string()
                } else {
                    e.to_string()
                };
                sh.lock().unwrap().error = Some(msg);
                ctx.request_repaint();
            }
        });
        Preview { stop, handle: Some(handle), shared }
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

const EBUSY: i32 = 16;

fn capture(path: &PathBuf, stop: &AtomicBool, shared: &Mutex<Shared>, ctx: &egui::Context) -> std::io::Result<()> {
    let dev = Device::with_path(path)?;
    let mut fmt = dev.format()?;
    fmt.width = WIDTH;
    fmt.height = HEIGHT;
    fmt.fourcc = FourCC::new(b"YUYV");
    let mut fmt = dev.set_format(&fmt)?;
    if &fmt.fourcc.repr != b"YUYV" {
        fmt.fourcc = FourCC::new(b"MJPG");
        fmt = dev.set_format(&fmt)?;
    }
    shared.lock().unwrap().format = Some(format!("{} {}×{}", fmt.fourcc, fmt.width, fmt.height));

    let mut stream = Stream::with_buffers(&dev, Type::VideoCapture, 4)?;
    while !stop.load(Ordering::Relaxed) {
        let (buf, meta) = stream.next()?;
        let data = &buf[..(meta.bytesused as usize).min(buf.len())];
        let image = match &fmt.fourcc.repr {
            b"YUYV" => Some(yuyv_to_image(data, fmt.width as usize, fmt.height as usize)),
            _ => mjpeg_to_image(data),
        };
        if let Some(img) = image {
            let mut s = shared.lock().unwrap();
            s.frame = Some(img);
            s.error = None;
            drop(s);
            ctx.request_repaint();
        }
    }
    Ok(())
}

/// YUYV 4:2:2 (BT.601, faixa limitada) para RGB.
fn yuyv_to_image(data: &[u8], w: usize, h: usize) -> ColorImage {
    let mut rgb = Vec::with_capacity(w * h * 3);
    for px in data.chunks_exact(4).take(w * h / 2) {
        let (y0, u, y1, v) = (px[0] as f32, px[1] as f32 - 128.0, px[2] as f32, px[3] as f32 - 128.0);
        for y in [y0, y1] {
            let c = 1.164 * (y - 16.0);
            rgb.push((c + 1.596 * v).clamp(0.0, 255.0) as u8);
            rgb.push((c - 0.392 * u - 0.813 * v).clamp(0.0, 255.0) as u8);
            rgb.push((c + 2.017 * u).clamp(0.0, 255.0) as u8);
        }
    }
    rgb.resize(w * h * 3, 0);
    ColorImage::from_rgb([w, h], &rgb)
}

fn mjpeg_to_image(data: &[u8]) -> Option<ColorImage> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(data, opts);
    let pixels = dec.decode().ok()?;
    let (w, h) = dec.dimensions()?;
    Some(ColorImage::from_rgb([w, h], &pixels))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yuyv_gray_and_size() {
        // Y=128, U=V=128: cinza médio, sem cor.
        let img = yuyv_to_image(&[128, 128, 128, 128], 2, 1);
        assert_eq!(img.size, [2, 1]);
        let p = img.pixels[0];
        assert!(p.r() == p.g() && p.g() == p.b());
    }
}
