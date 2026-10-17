//! Thread dona do dispositivo: a UI manda comandos e recebe fotografias do estado,
//! então um timeout de USB nunca congela a janela. Comandos em rajada (slider arrastado)
//! são agrupados e só o último valor vai para a câmera.

use eframe::egui;
use emeetctl_core::{Control, DeviceInfo, Light, LightStatus, Mic, MicStatus, Mode, Target, Video};
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
    /// Ganho do microfone em dB.
    MicGain(f32),
    MicOn(bool),
    /// Microfone da câmera como entrada padrão do sistema.
    MicDefault,
    /// Liga/desliga o serviço da câmera virtual (`systemctl --user start|stop`).
    VirtualCam(bool),
    /// Inicia a câmera virtual junto com a sessão (`systemctl --user enable|disable`).
    VirtualCamAutostart(bool),
}

/// Estado da câmera virtual visto pela interface.
#[derive(Clone, Default)]
pub struct VirtualCam {
    /// O serviço `emeetctl-virtualcam` está instalado para o usuário.
    pub installed: bool,
    pub active: bool,
    pub enabled: bool,
    /// Saídas já criadas, em ordem ("emeetctl Camera", "emeetctl Camera 2", ...).
    pub outputs: Vec<VirtualOutput>,
}

#[derive(Clone, Default)]
pub struct VirtualOutput {
    pub label: String,
    /// Nó `/dev/videoN`.
    pub device: String,
    /// Programas lendo esta saída (fora o próprio emeetctl).
    pub users: Option<String>,
}

const UNIT: &str = "emeetctl-virtualcam.service";

fn systemctl(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| format!("systemctl: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() || args[0].starts_with("is-") {
        Ok(text)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn virtualcam_state() -> VirtualCam {
    let enabled = systemctl(&["is-enabled", UNIT]).unwrap_or_default();
    VirtualCam {
        installed: !enabled.is_empty() && enabled != "not-found",
        active: systemctl(&["is-active", UNIT]).is_ok_and(|s| s == "active"),
        enabled: enabled == "enabled",
        outputs: emeetctl_core::virtualcam::find_devices()
            .into_iter()
            .enumerate()
            .map(|(i, p)| VirtualOutput {
                label: emeetctl_core::virtualcam::label(i),
                users: emeetctl_core::describe_users(&emeetctl_core::camera_users(&p)),
                device: p.display().to_string(),
            })
            .collect(),
    }
}

pub struct Snapshot {
    pub info: Option<DeviceInfo>,
    pub light: Result<LightStatus, String>,
    pub controls: Result<Vec<Control>, String>,
    pub error: Option<String>,
    pub virtualcam: VirtualCam,
    pub mic: Result<MicStatus, String>,
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
                Cmd::MicGain(db) => {
                    mic().and_then(|m| m.set_gain(db).map(|_| ()).map_err(|e| e.to_string()))
                }
                Cmd::MicOn(on) => mic().and_then(|m| m.set_on(on).map_err(|e| e.to_string())),
                Cmd::MicDefault => mic().and_then(|m| m.make_default().map_err(|e| e.to_string())),
                Cmd::VirtualCam(on) => {
                    systemctl(&[if on { "start" } else { "stop" }, UNIT]).map(|_| ())
                }
                Cmd::VirtualCamAutostart(on) => {
                    systemctl(&[if on { "enable" } else { "disable" }, UNIT]).map(|_| ())
                }
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
                virtualcam: virtualcam_state(),
                mic: mic().and_then(|m| m.status().map_err(|e| e.to_string())),
            })
            .is_err()
        {
            return;
        }
        ctx.request_repaint();
    }
}

/// O mixer é barato de abrir: abre a cada uso, e a câmera pode sair e voltar.
fn mic() -> Result<Mic, String> {
    Mic::open().map_err(|e| e.to_string())
}

/// Mantém a ordem, mas descarta ganhos, brilhos e valores de controle superados por um mais novo.
fn coalesce(cmds: Vec<Cmd>) -> Vec<Cmd> {
    let last_brightness = cmds.iter().rposition(|c| matches!(c, Cmd::Brightness(_)));
    let last_gain = cmds.iter().rposition(|c| matches!(c, Cmd::MicGain(_)));
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
            Cmd::MicGain(_) => Some(*i) == last_gain,
            Cmd::Ctrl(id, _) => last_ctrl.get(id) == Some(i),
            _ => true,
        })
        .map(|(_, c)| c)
        .collect()
}
