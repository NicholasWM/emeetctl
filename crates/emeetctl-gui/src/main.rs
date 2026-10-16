//! Interface gráfica do emeetctl (egui): prévia ao vivo à esquerda, painel de controles à direita.

mod preview;
mod theme;
mod worker;

use eframe::egui::{self, Color32, RichText};
use emeetctl_core::{light, Control, ControlKind, Mode, Target};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use theme::{card, segmented, slider_row, swatch, switch_row, ACCENT, LIVE, MUTED, WARN};
use worker::{Cmd, Snapshot};

/// Tempo em que a UI confia no valor local depois de mexer, antes de aceitar o da câmera.
const EDIT_GRACE: Duration = Duration::from_millis(800);
/// Largura a partir da qual a prévia vai para o lado.
const WIDE: f32 = 860.0;
const SIDE_WIDTH: f32 = 420.0;

/// Controle automático que trava um manual: ligar o auto esmaece o manual.
struct AutoPair {
    auto_key: &'static str,
    manual_key: &'static str,
    manual_label: &'static str,
}

const PAIRS: [AutoPair; 3] = [
    AutoPair { auto_key: "auto_exposure", manual_key: "exposure_time_absolute", manual_label: "Tempo de exposição" },
    AutoPair { auto_key: "white_balance_automatic", manual_key: "white_balance_temperature", manual_label: "Temperatura (K)" },
    AutoPair { auto_key: "focus_automatic_continuous", manual_key: "focus_absolute", manual_label: "Foco manual" },
];

/// Cartões da aba Imagem: título e as chaves (as dos pares entram pelo `auto_key`).
const GROUPS: [(&str, &[&str]); 5] = [
    ("Exposição", &["auto_exposure", "gain", "backlight_compensation"]),
    ("Balanço de branco", &["white_balance_automatic"]),
    ("Imagem", &["brightness", "contrast", "saturation", "hue", "gamma", "sharpness"]),
    ("Foco e zoom", &["focus_automatic_continuous", "zoom_absolute"]),
    ("Anti-flicker", &["power_line_frequency"]),
];

/// Zoom contínuo é velocidade, não posição: num slider ele ficaria dando zoom sozinho.
const HIDDEN: [&str; 1] = ["zoom_continuous"];

fn label_pt(key: &str) -> Option<&'static str> {
    Some(match key {
        "brightness" => "Brilho",
        "contrast" => "Contraste",
        "saturation" => "Saturação",
        "hue" => "Matiz",
        "gamma" => "Gama",
        "gain" => "Ganho",
        "sharpness" => "Nitidez",
        "backlight_compensation" => "Compensação de contraluz",
        "zoom_absolute" => "Zoom",
        _ => return None,
    })
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
    hex_edited: Option<Instant>,
    brightness: i32,
    brightness_edited: Option<Instant>,
    ctrl_values: HashMap<u32, i32>,
    ctrl_edited: HashMap<u32, Instant>,
    preview_on: bool,
    preview: Option<preview::Preview>,
    texture: Option<egui::TextureHandle>,
    preview_format: Option<String>,
    preview_error: Option<String>,
    last_error: Option<(String, Instant)>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> App {
        theme::apply(&cc.egui_ctx);
        let (tx, wrx) = mpsc::channel();
        let (wtx, rx) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || worker::worker(wrx, wtx, ctx));
        let _ = tx.send(Cmd::Refresh);
        App {
            tx,
            rx,
            tab: initial_tab(),
            snap: None,
            color: [0xFF, 0x00, 0x00],
            hex: "ff0000".into(),
            hex_edited: None,
            brightness: 100,
            brightness_edited: None,
            ctrl_values: HashMap::new(),
            ctrl_edited: HashMap::new(),
            preview_on: true,
            preview: None,
            texture: None,
            preview_format: None,
            preview_error: None,
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
                self.brightness = p as i32;
            }
            let typing = self.hex_edited.is_some_and(|t| t.elapsed() < Duration::from_secs(10));
            if let (Some(c), Some(Mode::Ambient), false) =
                (l.color.as_deref().and_then(|h| light::parse_hex(h).ok()), l.mode, typing)
            {
                self.color = c;
                self.hex = light::hex(c);
            }
        }
        if let Ok(ctrls) = &s.controls {
            for c in ctrls {
                match self.ctrl_edited.get(&c.id) {
                    Some(t) if t.elapsed() < EDIT_GRACE => {}
                    _ => {
                        self.ctrl_values.insert(c.id, c.value);
                    }
                }
            }
        }
        self.snap = Some(s);
    }

    fn controls(&self) -> Vec<Control> {
        match self.snap.as_ref().map(|s| &s.controls) {
            Some(Ok(c)) => c.clone(),
            _ => Vec::new(),
        }
    }

    fn set_ctrl(&mut self, c: &Control, v: i32) {
        self.ctrl_values.insert(c.id, v);
        self.ctrl_edited.insert(c.id, Instant::now());
        self.send(Cmd::Ctrl(c.id, v));
    }

    fn value(&self, c: &Control) -> i32 {
        *self.ctrl_values.get(&c.id).unwrap_or(&c.value)
    }

    // ---------- prévia ----------

    fn sync_preview(&mut self, ctx: &egui::Context) {
        let video = self.snap.as_ref().and_then(|s| s.info.as_ref()).and_then(|i| i.video.clone());
        match (self.preview_on, video, self.preview.is_some()) {
            (true, Some(path), false) if self.preview_error.is_none() => {
                self.preview = Some(preview::Preview::start(path.into(), ctx.clone()))
            }
            (false, _, true) | (true, None, true) => {
                self.preview = None;
                self.texture = None;
            }
            _ => {}
        }
        if let Some(p) = &self.preview {
            let mut s = p.shared.lock().unwrap();
            if let Some(img) = s.frame.take() {
                match &mut self.texture {
                    Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                    None => self.texture = Some(ctx.load_texture("preview", img, egui::TextureOptions::LINEAR)),
                }
            }
            self.preview_format = s.format.clone().map(|f| match s.fps {
                fps if fps > 0.0 => format!("{f} · {fps:.0} fps"),
                _ => f,
            });
            if let Some(e) = s.error.take() {
                self.preview_error = Some(e);
                drop(s);
                self.preview = None;
                self.texture = None;
            }
        }
    }

    fn preview_view(&mut self, ui: &mut egui::Ui, max: egui::Vec2) {
        let size = fit_16_9(max);
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 14.0, Color32::BLACK);

        match (&self.texture, self.preview_on) {
            (Some(t), true) => {
                egui::Image::new(t).corner_radius(14.0).paint_at(ui, rect);
                theme::chip(ui, rect.min + egui::vec2(12.0, 12.0), "AO VIVO", Some(LIVE));
                if let Some(f) = &self.preview_format {
                    let pos = egui::pos2(rect.left() + 12.0, rect.bottom() - 36.0);
                    theme::chip(ui, pos, f, None);
                }
            }
            _ => {
                let (msg, color) = match (&self.preview_error, self.preview_on) {
                    (Some(e), _) => (format!("Sem prévia: {e}"), WARN),
                    (None, false) => ("Prévia pausada".to_string(), MUTED),
                    (None, true) => ("Abrindo a câmera…".to_string(), MUTED),
                };
                painter.text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(15.0), color);
            }
        }

        let label = if self.preview_on { "Pausar prévia" } else { "Mostrar prévia" };
        let btn = egui::Rect::from_min_size(egui::pos2(rect.right() - 142.0, rect.top() + 10.0), egui::vec2(132.0, 30.0));
        if ui.put(btn, egui::Button::new(label).fill(Color32::from_black_alpha(170))).clicked() {
            self.preview_on = !self.preview_on;
            self.preview_error = None;
        }
    }

    // ---------- abas ----------

    fn light_tab(&mut self, ui: &mut egui::Ui) {
        let st = match self.snap.as_ref().map(|s| &s.light) {
            Some(Ok(st)) => st.clone(),
            other => {
                let err = other.and_then(|r| r.as_ref().err().cloned());
                return self.unavailable(ui, err);
            }
        };

        card(ui, "Energia", |ui| {
            let mut fill = st.fill_light.unwrap_or(false);
            if switch_row(ui, "Luz de preenchimento", Some("Fill Light: toda a luz do anel"), &mut fill) {
                self.send(Cmd::Power(Target::Fill, fill));
            }
            ui.separator();
            let mut amb = st.ambient.unwrap_or(false);
            if switch_row(ui, "Anel RGB", Some("Ambient: a cor do anel"), &mut amb) {
                self.send(Cmd::Power(Target::Ambient, amb));
            }
        });

        card(ui, "Modo", |ui| {
            let modes = [Mode::Cool, Mode::Warm, Mode::Balanced, Mode::Ambient, Mode::Rainbow];
            let sel = st.mode.and_then(|m| modes.iter().position(|x| *x == m));
            if let Some(i) = segmented(ui, &["Frio", "Quente", "Neutro", "Cor", "Arco-íris"], sel) {
                self.send(Cmd::Mode(modes[i]));
            }
        });

        card(ui, "Cor do anel", |ui| {
            let fixed = st.mode == Some(Mode::Ambient);
            ui.horizontal_wrapped(|ui| {
                for (name, rgb) in light::APP_PRESETS {
                    if swatch(ui, rgb, fixed && self.color == rgb, name).clicked() {
                        self.color = rgb;
                        self.hex = light::hex(rgb);
                        self.send(Cmd::Color(rgb));
                    }
                }
            });
            ui.horizontal(|ui| {
                if ui.color_edit_button_srgb(&mut self.color).changed() {
                    self.hex = light::hex(self.color);
                    self.send(Cmd::Color(self.color));
                }
                ui.label(RichText::new("#").color(MUTED));
                let r = ui.add(egui::TextEdit::singleline(&mut self.hex).desired_width(84.0).char_limit(7).font(egui::TextStyle::Monospace));
                if r.changed() {
                    self.hex_edited = Some(Instant::now());
                }
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if enter || ui.button("Aplicar").clicked() {
                    match light::parse_hex(&self.hex) {
                        Ok(c) => {
                            self.color = c;
                            self.hex_edited = None;
                            self.send(Cmd::Color(c));
                        }
                        Err(e) => self.last_error = Some((e.to_string(), Instant::now())),
                    }
                }
            });
            if !fixed {
                ui.label(RichText::new("Escolher uma cor muda o modo para Cor.").small().color(MUTED));
            }
        });

        card(ui, "Brilho", |ui| {
            if slider_row(ui, "Intensidade", &mut self.brightness, 0..=100, 1, true, " %") {
                self.brightness_edited = Some(Instant::now());
                self.send(Cmd::Brightness(self.brightness as u8));
            }
        });
    }

    fn image_tab(&mut self, ui: &mut egui::Ui) {
        let ctrls = self.controls();
        if ctrls.is_empty() {
            let err = self.snap.as_ref().and_then(|s| s.controls.as_ref().err().cloned());
            return self.unavailable(ui, err);
        }
        let by_key: HashMap<&str, &Control> = ctrls.iter().map(|c| (c.key.as_str(), c)).collect();
        let mut shown: Vec<&str> = Vec::new();

        for (title, keys) in GROUPS {
            if !keys.iter().any(|k| by_key.contains_key(k)) {
                continue;
            }
            card(ui, title, |ui| {
                for key in keys {
                    let Some(c) = by_key.get(key) else { continue };
                    shown.push(key);
                    if let Some(pair) = PAIRS.iter().find(|p| p.auto_key == *key) {
                        self.auto_pair(ui, c, pair, by_key.get(pair.manual_key).copied());
                        shown.push(pair.manual_key);
                    } else {
                        self.control_widget(ui, c);
                    }
                    if *key == "zoom_absolute" {
                        ui.label(RichText::new("A câmera ignora o zoom em 1080p, 4K e no formato YUYV.").small().color(MUTED));
                    }
                }
            });
        }

        let rest: Vec<&Control> = ctrls
            .iter()
            .filter(|c| !shown.contains(&c.key.as_str()) && !HIDDEN.contains(&c.key.as_str()))
            .collect();
        if !rest.is_empty() {
            card(ui, "Outros", |ui| {
                for c in rest {
                    self.control_widget(ui, c);
                }
            });
        }

        ui.add_space(4.0);
        if ui.add(egui::Button::new("Restaurar padrões de fábrica").min_size(egui::vec2(ui.available_width(), 34.0))).clicked() {
            self.ctrl_edited.clear();
            self.send(Cmd::ResetCtrls);
        }
    }

    /// Switch "Automático" e, embaixo, o controle manual que ele destrava.
    fn auto_pair(&mut self, ui: &mut egui::Ui, auto: &Control, pair: &AutoPair, manual: Option<&Control>) {
        let v = self.value(auto);
        let (mut on, on_value, off_value) = match &auto.kind {
            // auto_exposure é menu: "Manual Mode" desliga, o outro item liga.
            ControlKind::Menu { items } => {
                let manual_item = items.iter().find(|(_, n)| n.to_lowercase().contains("manual")).map(|(i, _)| *i);
                let auto_item = items.iter().find(|(i, _)| Some(*i) != manual_item).map(|(i, _)| *i);
                (Some(v) != manual_item, auto_item.unwrap_or(auto.default), manual_item.unwrap_or(1))
            }
            _ => (v != 0, 1, 0),
        };
        if switch_row(ui, "Automático", None, &mut on) {
            self.set_ctrl(auto, if on { on_value } else { off_value });
        }
        if let Some(m) = manual {
            let mut mv = self.value(m);
            if slider_row(ui, pair.manual_label, &mut mv, m.min..=m.max, m.step, !on, "") {
                self.set_ctrl(m, mv);
            }
        }
        ui.add_space(2.0);
    }

    fn control_widget(&mut self, ui: &mut egui::Ui, c: &Control) {
        let label = label_pt(&c.key).unwrap_or(&c.name);
        let mut v = self.value(c);
        let enabled = !c.inactive && !c.read_only;
        let changed = match &c.kind {
            ControlKind::Boolean => {
                let mut b = v != 0;
                let r = switch_row(ui, label, None, &mut b);
                v = b as i32;
                r
            }
            ControlKind::Menu { items } => {
                let labels: Vec<String> = items.iter().map(|(_, n)| menu_pt(n)).collect();
                let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                let sel = items.iter().position(|(i, _)| *i == v);
                match segmented(ui, &refs, sel) {
                    Some(i) => {
                        v = items[i].0;
                        true
                    }
                    None => false,
                }
            }
            ControlKind::Integer => slider_row(ui, label, &mut v, c.min..=c.max, c.step, enabled, ""),
        };
        if changed {
            self.set_ctrl(c, v);
        }
    }

    fn device_tab(&self, ui: &mut egui::Ui) {
        let Some(info) = self.snap.as_ref().and_then(|s| s.info.clone()) else {
            return self.unavailable(ui, Some("câmera não encontrada".into()));
        };
        card(ui, "Dispositivo", |ui| {
            egui::Grid::new("info").num_columns(2).spacing([18.0, 10.0]).show(ui, |ui| {
                for (k, v) in [
                    ("Produto", info.product),
                    ("Serial", info.serial),
                    ("Firmware", info.firmware),
                    ("Canal da luz", info.hidraw.unwrap_or("—".into())),
                    ("Vídeo", info.video.unwrap_or("—".into())),
                ] {
                    ui.label(RichText::new(k).color(MUTED));
                    ui.label(RichText::new(v).monospace());
                    ui.end_row();
                }
            });
        });
        card(ui, "Firmware", |ui| {
            ui.label(
                RichText::new("Atualização de firmware não é suportada, de propósito: o canal da câmera pode ter comandos de firmware, e o emeetctl só envia os comandos de luz mapeados.")
                    .color(MUTED),
            );
        });
    }

    fn unavailable(&self, ui: &mut egui::Ui, err: Option<String>) {
        card(ui, "Câmera indisponível", |ui| {
            if let Some(e) = &err {
                ui.label(e);
                if e.contains("permissão") {
                    ui.add_space(6.0);
                    ui.code("sudo cp udev/70-emeetctl.rules /etc/udev/rules.d/\nsudo udevadm control --reload\nsudo udevadm trigger --subsystem-match=hidraw --action=change");
                }
            }
            if ui.button("Tentar de novo").clicked() {
                self.send(Cmd::Refresh);
            }
        });
    }

    fn side(&mut self, ui: &mut egui::Ui) {
        let tabs = [Tab::Light, Tab::Image, Tab::Device];
        let sel = tabs.iter().position(|t| *t == self.tab);
        if let Some(i) = segmented(ui, &["Luz", "Imagem", "Dispositivo"], sel) {
            self.tab = tabs[i];
        }
        ui.add_space(4.0);
        match self.tab {
            Tab::Light => self.light_tab(ui),
            Tab::Image => self.image_tab(ui),
            Tab::Device => self.device_tab(ui),
        }
    }

    fn header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top")
            .frame(egui::Frame::default().fill(theme::BG).inner_margin(egui::Margin::symmetric(18, 12)))
            .show_separator_line(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("emeetctl").size(19.0).strong());
                    ui.add_space(6.0);
                    let (dot, text) = match &self.snap {
                        Some(s) if s.info.is_some() => {
                            let i = s.info.as_ref().unwrap();
                            (ACCENT, format!("{} · firmware {}", i.product, i.firmware))
                        }
                        Some(_) => (LIVE, "câmera desconectada".into()),
                        None => (MUTED, "procurando…".into()),
                    };
                    let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.5, dot);
                    ui.label(RichText::new(text).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("⟳  Atualizar").on_hover_text("Ler a câmera de novo").clicked() {
                            self.send(Cmd::Refresh);
                        }
                    });
                });
            });
    }

    fn footer(&mut self, ctx: &egui::Context) {
        let Some((e, t)) = &self.last_error else { return };
        if t.elapsed() > Duration::from_secs(8) {
            return;
        }
        egui::TopBottomPanel::bottom("status")
            .frame(egui::Frame::default().fill(theme::BG).inner_margin(egui::Margin::symmetric(18, 8)))
            .show(ctx, |ui| {
                ui.label(RichText::new(format!("⚠  {e}")).color(WARN));
            });
        ctx.request_repaint_after(Duration::from_secs(1));
    }
}

/// `--tab luz|imagem|dispositivo` abre direto numa aba.
fn initial_tab() -> Tab {
    let args: Vec<String> = std::env::args().collect();
    match args.iter().position(|a| a == "--tab").and_then(|i| args.get(i + 1)).map(String::as_str) {
        Some("imagem") => Tab::Image,
        Some("dispositivo") => Tab::Device,
        _ => Tab::Light,
    }
}

fn menu_pt(name: &str) -> String {
    match name {
        "Disabled" => "Desligado".into(),
        other => other.to_string(),
    }
}

fn fit_16_9(max: egui::Vec2) -> egui::Vec2 {
    let h = (max.x * 9.0 / 16.0).min(max.y);
    egui::vec2(h * 16.0 / 9.0, h)
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(s) = self.rx.try_recv() {
            self.absorb(s);
        }
        self.sync_preview(ctx);
        self.header(ctx);
        self.footer(ctx);

        let pad = egui::Margin::same(18);
        if ctx.screen_rect().width() >= WIDE {
            egui::SidePanel::right("side")
                .exact_width(SIDE_WIDTH)
                .resizable(false)
                .frame(egui::Frame::default().fill(theme::PANEL).inner_margin(pad))
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.side(ui));
                });
            egui::CentralPanel::default()
                .frame(egui::Frame::default().fill(theme::BG).inner_margin(pad))
                .show(ctx, |ui| {
                    let max = ui.available_size();
                    ui.vertical_centered(|ui| {
                        let size = fit_16_9(max);
                        ui.add_space(((max.y - size.y) / 2.0).max(0.0));
                        self.preview_view(ui, max);
                    });
                });
        } else {
            egui::CentralPanel::default()
                .frame(egui::Frame::default().fill(theme::PANEL).inner_margin(pad))
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                        let max = egui::vec2(ui.available_width(), 320.0);
                        self.preview_view(ui, max);
                        ui.add_space(6.0);
                        self.side(ui);
                    });
                });
        }
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("emeetctl")
            .with_app_id("emeetctl")
            .with_inner_size([1180.0, 720.0])
            .with_min_inner_size([420.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native("emeetctl", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
