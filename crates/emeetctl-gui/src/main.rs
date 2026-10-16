//! Interface gráfica do emeetctl (egui).
//!
//! Uma thread de trabalho é dona do dispositivo: a UI só manda comandos e recebe estado,
//! então um timeout de USB nunca congela a janela. Comandos em rajada (slider arrastado)
//! são agrupados e só o último valor vai para a câmera.

mod preview;

use eframe::egui::{self, Color32, RichText};
use emeetctl_core::{light, Control, ControlKind, DeviceInfo, Light, LightStatus, Mode, Target, Video};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// Controles que não fazem sentido como slider: zoom contínuo é velocidade, não posição.
const HIDDEN_CONTROLS: [&str; 1] = ["zoom_continuous"];

/// Nome em português dos controles UVC conhecidos; o resto mostra o nome do kernel.
fn label_pt(key: &str) -> Option<&'static str> {
    Some(match key {
        "brightness" => "Brilho",
        "contrast" => "Contraste",
        "saturation" => "Saturação",
        "hue" => "Matiz",
        "gamma" => "Gama",
        "gain" => "Ganho",
        "sharpness" => "Nitidez",
        "white_balance_automatic" => "Balanço de branco automático",
        "white_balance_temperature" => "Temperatura de cor (K)",
        "power_line_frequency" => "Anti-flicker",
        "backlight_compensation" => "Compensação de contraluz",
        "auto_exposure" => "Exposição",
        "exposure_time_absolute" => "Tempo de exposição",
        "focus_automatic_continuous" => "Autofoco",
        "focus_absolute" => "Foco manual",
        "zoom_absolute" => "Zoom",
        _ => return None,
    })
}
/// Tempo em que a UI confia no valor local depois de mexer, antes de aceitar o da câmera.
const EDIT_GRACE: Duration = Duration::from_millis(800);
const POLL: Duration = Duration::from_secs(5);

enum Cmd {
    Refresh,
    Power(Target, bool),
    Mode(Mode),
    Color([u8; 3]),
    Brightness(u8),
    Ctrl(u32, i32),
    ResetCtrls,
}

struct Snapshot {
    info: Option<DeviceInfo>,
    light: Result<LightStatus, String>,
    controls: Result<Vec<Control>, String>,
    error: Option<String>,
}

fn worker(rx: Receiver<Cmd>, tx: Sender<Snapshot>, ctx: egui::Context) {
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
                let h = p.hidraw.as_ref().ok_or("canal HID da câmera não encontrado")?;
                *l = Some(Light::open(h).map_err(|e| e.to_string())?);
            }
            Ok(())
        };
        let open_video = |v: &mut Option<Video>| -> Result<(), String> {
            if v.is_none() {
                let p = paths.as_ref().map_err(|e| e.to_string())?;
                let n = p.video.as_ref().ok_or("nó de vídeo da câmera não encontrado")?;
                *v = Some(Video::open(n).map_err(|e| e.to_string())?);
            }
            Ok(())
        };

        let mut error = None;
        for cmd in batch {
            let r: Result<(), String> = match cmd {
                Cmd::Refresh => Ok(()),
                Cmd::Power(t, on) => open_light(&mut light)
                    .and_then(|_| light.as_mut().unwrap().set_power(t, on).map_err(|e| e.to_string())),
                Cmd::Mode(m) => open_light(&mut light)
                    .and_then(|_| light.as_mut().unwrap().set_mode(m).map_err(|e| e.to_string())),
                Cmd::Color(c) => open_light(&mut light)
                    .and_then(|_| light.as_mut().unwrap().set_color(c).map_err(|e| e.to_string())),
                Cmd::Brightness(p) => open_light(&mut light).and_then(|_| {
                    light.as_mut().unwrap().set_brightness_percent(p).map(|_| ()).map_err(|e| e.to_string())
                }),
                Cmd::Ctrl(id, v) => open_video(&mut video)
                    .and_then(|_| video.as_ref().unwrap().set(id, v).map_err(|e| e.to_string())),
                Cmd::ResetCtrls => open_video(&mut video)
                    .and_then(|_| video.as_ref().unwrap().reset_defaults().map(|_| ()).map_err(|e| e.to_string())),
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
        let controls = open_video(&mut video)
            .and_then(|_| video.as_ref().unwrap().controls().map_err(|e| e.to_string()));
        if controls.is_err() {
            video = None;
        }
        if tx.send(Snapshot { info, light: light_state, controls, error }).is_err() {
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

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Light,
    Image,
    Device,
}

struct App {
    tx: Sender<Cmd>,
    rx: Receiver<Snapshot>,
    tab: Tab,
    snap: Option<Snapshot>,
    color: [u8; 3],
    hex: String,
    brightness: f32,
    brightness_edited: Option<Instant>,
    ctrl_values: HashMap<u32, i32>,
    ctrl_edited: HashMap<u32, Instant>,
    hex_edited: Option<Instant>,
    preview_on: bool,
    preview: Option<preview::Preview>,
    texture: Option<egui::TextureHandle>,
    last_error: Option<(String, Instant)>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> App {
        let (tx, wrx) = mpsc::channel();
        let (wtx, rx) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || worker(wrx, wtx, ctx));
        let _ = tx.send(Cmd::Refresh);
        App {
            tx,
            rx,
            tab: Tab::Light,
            snap: None,
            color: [0xFF, 0x00, 0x00],
            hex: "ff0000".into(),
            brightness: 100.0,
            brightness_edited: None,
            ctrl_values: HashMap::new(),
            ctrl_edited: HashMap::new(),
            hex_edited: None,
            preview_on: true,
            preview: None,
            texture: None,
            last_error: None,
        }
    }

    fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }

    fn absorb(&mut self, s: Snapshot) {
        if let Some(e) = &s.error {
            self.last_error = Some((e.clone(), Instant::now()));
        }
        if let Ok(l) = &s.light {
            let editing = self.brightness_edited.is_some_and(|t| t.elapsed() < EDIT_GRACE);
            if let (Some(p), false) = (l.brightness_percent, editing) {
                self.brightness = p as f32;
            }
            if let Some(c) = l.color.as_deref().and_then(|h| light::parse_hex(h).ok()) {
                if l.mode == Some(Mode::Ambient) && self.hex_edited.is_none_or(|t| t.elapsed() >= Duration::from_secs(10)) {
                    self.color = c;
                    self.hex = light::hex(c);
                }
            }
        }
        if let Ok(ctrls) = &s.controls {
            for c in ctrls {
                let editing = self.ctrl_edited.get(&c.id).is_some_and(|t| t.elapsed() < EDIT_GRACE);
                if !editing {
                    self.ctrl_values.insert(c.id, c.value);
                }
            }
        }
        self.snap = Some(s);
    }

    /// Abre ou fecha o stream conforme o botão e a presença da câmera.
    fn sync_preview(&mut self, ctx: &egui::Context) {
        let video = self.snap.as_ref().and_then(|s| s.info.as_ref()).and_then(|i| i.video.clone());
        match (self.preview_on, video, self.preview.is_some()) {
            (true, Some(path), false) => self.preview = Some(preview::Preview::start(path.into(), ctx.clone())),
            (false, _, true) | (true, None, true) => {
                self.preview = None;
                self.texture = None;
            }
            _ => {}
        }
    }

    fn preview_panel(&mut self, ui: &mut egui::Ui) {
        if !self.preview_on {
            return;
        }
        let Some(p) = &self.preview else { return };
        let (frame, error, format) = {
            let mut s = p.shared.lock().unwrap();
            (s.frame.take(), s.error.clone(), s.format.clone())
        };
        if let Some(img) = frame {
            match &mut self.texture {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => self.texture = Some(ui.ctx().load_texture("preview", img, egui::TextureOptions::LINEAR)),
            }
        }
        let w = ui.available_width();
        let h = (w * 3.0 / 4.0).min(ui.ctx().screen_rect().height() * 0.42);
        let size = egui::vec2(h * 4.0 / 3.0, h);
        ui.vertical_centered(|ui| match (&self.texture, error) {
            (_, Some(e)) => {
                ui.add_sized(size, egui::Label::new(RichText::new(format!("Sem prévia: {e}")).weak()));
                if ui.button("Tentar de novo").clicked() {
                    self.preview = None;
                }
            }
            (Some(t), None) => {
                ui.add(egui::Image::new(t).fit_to_exact_size(size).corner_radius(6.0));
                if let Some(f) = format {
                    ui.label(RichText::new(f).small().weak());
                }
            }
            (None, None) => {
                ui.add_sized(size, egui::Spinner::new());
            }
        });
        ui.separator();
    }

    fn light_tab(&mut self, ui: &mut egui::Ui) {
        let Some(Ok(st)) = self.snap.as_ref().map(|s| s.light.clone()) else {
            self.unavailable(ui, self.snap.as_ref().and_then(|s| s.light.as_ref().err().cloned()));
            return;
        };

        section(ui, "Energia");
        ui.horizontal(|ui| {
            let mut fill = st.fill_light.unwrap_or(false);
            if toggle(ui, &mut fill, "Fill Light").changed() {
                self.send(Cmd::Power(Target::Fill, fill));
            }
            ui.add_space(24.0);
            let mut amb = st.ambient.unwrap_or(false);
            if toggle(ui, &mut amb, "Anel RGB").changed() {
                self.send(Cmd::Power(Target::Ambient, amb));
            }
        });

        section(ui, "Modo");
        ui.horizontal_wrapped(|ui| {
            for (m, label) in [
                (Mode::Cool, "Frio"),
                (Mode::Warm, "Quente"),
                (Mode::Balanced, "Equilibrado"),
                (Mode::Ambient, "Cor fixa"),
                (Mode::Rainbow, "Arco-íris"),
            ] {
                if ui.selectable_label(st.mode == Some(m), label).clicked() {
                    self.send(Cmd::Mode(m));
                }
            }
        });

        section(ui, "Cor do anel");
        ui.horizontal_wrapped(|ui| {
            for (name, rgb) in light::APP_PRESETS {
                let swatch = egui::Button::new("    ")
                    .fill(Color32::from_rgb(rgb[0], rgb[1], rgb[2]))
                    .min_size(egui::vec2(36.0, 28.0));
                if ui.add(swatch).on_hover_text(name).clicked() {
                    self.color = rgb;
                    self.hex = light::hex(rgb);
                    self.send(Cmd::Color(rgb));
                }
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.color_edit_button_srgb(&mut self.color).changed() {
                self.hex = light::hex(self.color);
                self.send(Cmd::Color(self.color));
            }
            ui.label("#");
            let r = ui.add(egui::TextEdit::singleline(&mut self.hex).desired_width(70.0).char_limit(7));
            if r.changed() {
                self.hex_edited = Some(Instant::now());
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter || ui.button("Aplicar").clicked() {
                match light::parse_hex(&self.hex) {
                    Ok(c) => {
                        self.color = c;
                        self.send(Cmd::Color(c));
                    }
                    Err(e) => self.last_error = Some((e.to_string(), Instant::now())),
                }
            }
        });

        section(ui, "Brilho");
        let (lo, hi) = st.mode.map(|m| m.brightness_range()).unwrap_or((0, 255));
        let r = ui.add(egui::Slider::new(&mut self.brightness, 0.0..=100.0).suffix(" %").integer());
        if r.changed() {
            self.brightness_edited = Some(Instant::now());
            self.send(Cmd::Brightness(self.brightness.round() as u8));
        }
        ui.label(
            RichText::new(format!(
                "Faixa crua deste modo: 0x{lo:02x}–0x{hi:02x}. Atual: {}",
                st.brightness_raw.map(|v| format!("0x{v:02x}")).unwrap_or("?".into())
            ))
            .small()
            .weak(),
        );
    }

    fn image_tab(&mut self, ui: &mut egui::Ui) {
        let ctrls = match self.snap.as_ref().map(|s| &s.controls) {
            Some(Ok(c)) => c.clone(),
            other => {
                let err = other.and_then(|r| r.as_ref().err().cloned());
                self.unavailable(ui, err);
                return;
            }
        };
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("ctrls").num_columns(2).spacing([16.0, 10.0]).striped(true).show(ui, |ui| {
                for c in ctrls.iter().filter(|c| !HIDDEN_CONTROLS.contains(&c.key.as_str())) {
                    ui.label(label_pt(&c.key).unwrap_or(&c.name)).on_hover_text(format!("{} ({})", c.name, c.key));
                    let enabled = !c.inactive && !c.read_only;
                    let mut v = *self.ctrl_values.get(&c.id).unwrap_or(&c.value);
                    let changed = ui
                        .add_enabled_ui(enabled, |ui| match &c.kind {
                            ControlKind::Boolean => {
                                let mut b = v != 0;
                                let text = if b { "ligado" } else { "desligado" };
                                let r = ui.checkbox(&mut b, text);
                                v = b as i32;
                                r.changed()
                            }
                            ControlKind::Menu { items } => {
                                let cur = items.iter().find(|(i, _)| *i == v).map(|(_, n)| n.clone()).unwrap_or_default();
                                let mut changed = false;
                                egui::ComboBox::from_id_salt(c.id).selected_text(cur).show_ui(ui, |ui| {
                                    for (i, n) in items {
                                        changed |= ui.selectable_value(&mut v, *i, n).changed();
                                    }
                                });
                                changed
                            }
                            ControlKind::Integer => ui
                                .add(egui::Slider::new(&mut v, c.min..=c.max).step_by(c.step as f64))
                                .changed(),
                        })
                        .inner;
                    if changed {
                        self.ctrl_values.insert(c.id, v);
                        self.ctrl_edited.insert(c.id, Instant::now());
                        self.send(Cmd::Ctrl(c.id, v));
                    }
                    ui.end_row();
                }
            });
            ui.add_space(8.0);
            ui.label(RichText::new("Controles esmaecidos dependem de desligar o modo automático correspondente.").small().weak());
            ui.add_space(8.0);
            if ui.button("Restaurar padrões de fábrica").clicked() {
                self.ctrl_edited.clear();
                self.send(Cmd::ResetCtrls);
            }
        });
    }

    fn device_tab(&self, ui: &mut egui::Ui) {
        let Some(info) = self.snap.as_ref().and_then(|s| s.info.clone()) else {
            self.unavailable(ui, Some("câmera não encontrada".into()));
            return;
        };
        egui::Grid::new("info").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
            for (k, v) in [
                ("Produto", info.product),
                ("Serial", info.serial),
                ("Firmware", info.firmware),
                ("Canal da luz", info.hidraw.unwrap_or("—".into())),
                ("Vídeo", info.video.unwrap_or("—".into())),
            ] {
                ui.label(RichText::new(k).weak());
                ui.label(v);
                ui.end_row();
            }
        });
        ui.add_space(12.0);
        ui.label(
            RichText::new("Atualização de firmware não é suportada, de propósito: o canal pode ter comandos de firmware e o emeetctl só envia os comandos de luz mapeados.")
                .small()
                .weak(),
        );
    }

    fn unavailable(&self, ui: &mut egui::Ui, err: Option<String>) {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.heading("Câmera indisponível");
            if let Some(e) = err {
                ui.label(e.clone());
                if e.contains("permissão") {
                    ui.add_space(8.0);
                    ui.code("sudo cp udev/70-emeetctl.rules /etc/udev/rules.d/\nsudo udevadm control --reload\nsudo udevadm trigger --subsystem-match=hidraw --action=change");
                }
            }
            ui.add_space(8.0);
            if ui.button("Tentar de novo").clicked() {
                self.send(Cmd::Refresh);
            }
        });
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(14.0);
    ui.label(RichText::new(title).strong().size(15.0));
    ui.add_space(4.0);
}

fn toggle(ui: &mut egui::Ui, on: &mut bool, label: &str) -> egui::Response {
    ui.horizontal(|ui| {
        let r = ui.add(egui::Checkbox::new(on, ""));
        ui.label(label);
        r
    })
    .inner
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(s) = self.rx.try_recv() {
            self.absorb(s);
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("emeetctl");
                let (dot, text) = match &self.snap {
                    Some(s) if s.info.is_some() => (Color32::from_rgb(80, 200, 120), s.info.as_ref().unwrap().product.clone()),
                    Some(_) => (Color32::from_rgb(220, 90, 80), "câmera desconectada".into()),
                    None => (Color32::GRAY, "procurando…".into()),
                };
                let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.5, dot);
                ui.label(RichText::new(text).weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.toggle_value(&mut self.preview_on, "Prévia").on_hover_text("Mostrar o que a câmera está filmando");
                    if ui.button("⟳").on_hover_text("Ler a câmera de novo").clicked() {
                        self.send(Cmd::Refresh);
                    }
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Light, "Luz");
                ui.selectable_value(&mut self.tab, Tab::Image, "Imagem");
                ui.selectable_value(&mut self.tab, Tab::Device, "Dispositivo");
            });
            ui.add_space(4.0);
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            match &self.last_error {
                Some((e, t)) if t.elapsed() < Duration::from_secs(8) => {
                    ui.label(RichText::new(format!("⚠ {e}")).color(Color32::from_rgb(230, 120, 90)));
                    ctx.request_repaint_after(Duration::from_secs(1));
                }
                _ => {
                    ui.label(RichText::new("pronto").small().weak());
                }
            }
        });

        self.sync_preview(ctx);
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.tab != Tab::Device {
                self.preview_panel(ui);
            }
            match self.tab {
                Tab::Light => self.light_tab(ui),
                Tab::Image => self.image_tab(ui),
                Tab::Device => self.device_tab(ui),
            }
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("emeetctl")
            .with_app_id("emeetctl")
            .with_inner_size([520.0, 900.0])
            .with_min_inner_size([380.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native("emeetctl", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
