//! Thread dona do dispositivo: a UI manda comandos e recebe fotografias do estado,
//! então um timeout de USB nunca congela a janela. Comandos em rajada (slider arrastado)
//! são agrupados e só o último valor vai para a câmera.

use eframe::egui;
use emeetctl_core::{Control, DeviceInfo, Light, LightStatus, Mode, Target, Video};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

const POLL: Duration = Duration::from_secs(5);

pub enum Cmd {
    Refresh,
    Power(Target, bool),
    Mode(Mode),
    Color([u8; 3]),
    Brightness(u8),
    Ctrl(u32, i32),
    ResetCtrls,
}

pub struct Snapshot {
    pub info: Option<DeviceInfo>,
    pub light: Result<LightStatus, String>,
    pub controls: Result<Vec<Control>, String>,
    pub error: Option<String>,
}

pub fn worker(rx: Receiver<Cmd>, tx: Sender<Snapshot>, ctx: egui::Context) {
    let mut light: Option<Light> = None;
    let mut video: Option<Video> = None;
    loop {
        let first = match rx.recv_timeout(POLL) {
            Ok(c) => c,
            Err(RecvTimeoutError::Timeout) => Cmd::Refresh,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let batch = coalesce(std::iter::once(first).chain(rx.try_iter()).collect());

        let paths = emeetctl_core::find();
        if paths.is_err() {
            light = None;
            video = None;
        }
        let info = paths.as_ref().ok().map(|p| p.info());
        let open_light = |l: &mut Option<Light>| -> Result<(), String> {
            if l.is_none() {
                let p = paths.as_ref().map_err(|e| e.to_string())?;
                let h = p
                    .hidraw
                    .as_ref()
                    .ok_or("canal HID da câmera não encontrado")?;
                *l = Some(Light::open(h).map_err(|e| e.to_string())?);
            }
            Ok(())
        };
        let open_video = |v: &mut Option<Video>| -> Result<(), String> {
            if v.is_none() {
                let p = paths.as_ref().map_err(|e| e.to_string())?;
                let n = p
                    .video
                    .as_ref()
                    .ok_or("nó de vídeo da câmera não encontrado")?;
                *v = Some(Video::open(n).map_err(|e| e.to_string())?);
            }
            Ok(())
        };

        let mut error = None;
        for cmd in batch {
            let r: Result<(), String> = match cmd {
                Cmd::Refresh => Ok(()),
                Cmd::Power(t, on) => open_light(&mut light).and_then(|_| {
                    light
                        .as_mut()
                        .unwrap()
                        .set_power(t, on)
                        .map_err(|e| e.to_string())
                }),
                Cmd::Mode(m) => open_light(&mut light).and_then(|_| {
                    light
                        .as_mut()
                        .unwrap()
                        .set_mode(m)
                        .map_err(|e| e.to_string())
                }),
                Cmd::Color(c) => open_light(&mut light).and_then(|_| {
                    light
                        .as_mut()
                        .unwrap()
                        .set_color(c)
                        .map_err(|e| e.to_string())
                }),
                Cmd::Brightness(p) => open_light(&mut light).and_then(|_| {
                    light
                        .as_mut()
                        .unwrap()
                        .set_brightness_percent(p)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                }),
                Cmd::Ctrl(id, v) => open_video(&mut video).and_then(|_| {
                    video
                        .as_ref()
                        .unwrap()
                        .set(id, v)
                        .map_err(|e| e.to_string())
                }),
                Cmd::ResetCtrls => open_video(&mut video).and_then(|_| {
                    video
                        .as_ref()
                        .unwrap()
                        .reset_defaults()
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                }),
            };
            if let Err(e) = r {
                error = Some(e);
                // Handle pode ter morrido (desconectou): reabre na próxima.
                light = None;
                video = None;
            }
        }

        let light_state = open_light(&mut light)
            .and_then(|_| light.as_mut().unwrap().status().map_err(|e| e.to_string()));
        if light_state.is_err() {
            light = None;
        }
        let controls = open_video(&mut video).and_then(|_| {
            video
                .as_ref()
                .unwrap()
                .controls()
                .map_err(|e| e.to_string())
        });
        if controls.is_err() {
            video = None;
        }
        if tx
            .send(Snapshot {
                info,
                light: light_state,
                controls,
                error,
            })
            .is_err()
        {
            return;
        }
        ctx.request_repaint();
    }
}

/// Mantém a ordem, mas descarta brilhos e valores de controle superados por um mais novo.
fn coalesce(cmds: Vec<Cmd>) -> Vec<Cmd> {
    let last_brightness = cmds.iter().rposition(|c| matches!(c, Cmd::Brightness(_)));
    let mut last_ctrl: HashMap<u32, usize> = HashMap::new();
    for (i, c) in cmds.iter().enumerate() {
        if let Cmd::Ctrl(id, _) = c {
            last_ctrl.insert(*id, i);
        }
    }
    cmds.into_iter()
        .enumerate()
        .filter(|(i, c)| match c {
            Cmd::Brightness(_) => Some(*i) == last_brightness,
            Cmd::Ctrl(id, _) => last_ctrl.get(id) == Some(i),
            _ => true,
        })
        .map(|(_, c)| c)
        .collect()
}
