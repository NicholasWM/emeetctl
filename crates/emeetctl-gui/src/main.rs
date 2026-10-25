//! Interface gráfica do emeetctl (egui): prévia ao vivo à esquerda, painel de controles à direita.

mod preview;
mod theme;
mod worker;

use eframe::egui::{self, Color32, RichText};
use emeetctl_core::mic::{self, Meter};
use emeetctl_core::record::{self, Audio, Format, Options, Recorder};
use emeetctl_core::{light, Control, ControlKind, Mode, Target};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use theme::{
    card, segmented, slider_db, slider_row, swatch, switch_row, ACCENT, LIVE, MUTED, WARN,
};
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
    AutoPair {
        auto_key: "auto_exposure",
        manual_key: "exposure_time_absolute",
        manual_label: "Tempo de exposição",
    },
    AutoPair {
        auto_key: "white_balance_automatic",
        manual_key: "white_balance_temperature",
        manual_label: "Temperatura (K)",
    },
    AutoPair {
        auto_key: "focus_automatic_continuous",
        manual_key: "focus_absolute",
        manual_label: "Foco manual",
    },
];

/// Cartões da aba Imagem: título e as chaves (as dos pares entram pelo `auto_key`).
const GROUPS: [(&str, &[&str]); 5] = [
    (
        "Exposição",
        &["auto_exposure", "gain", "backlight_compensation"],
    ),
    ("Balanço de branco", &["white_balance_automatic"]),
    (
        "Imagem",
        &[
            "brightness",
            "contrast",
            "saturation",
            "hue",
            "gamma",
            "sharpness",
        ],
    ),
    (
        "Foco e zoom",
        &["focus_automatic_continuous", "zoom_absolute"],
    ),
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
    Mic,
    Record,
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
    /// A prévia só segura a câmera com a janela em foco: em segundo plano ela é
    /// liberada, para o Meet/Zoom conseguirem abrir (no Linux só um programa
    /// lê o stream da webcam por vez).
    focused: bool,
    /// Nó de onde a prévia está lendo agora (a câmera física ou a virtual).
    preview_src: Option<String>,
    last_error: Option<(String, Instant)>,
    mic_gain: i32,
    mic_gain_edited: Option<Instant>,
    phones: f32,
    phones_edited: Option<Instant>,
    /// Medidor de nível, só com a aba Microfone aberta e a janela em foco.
    meter: Option<(String, Meter)>,
    meter_error: Option<String>,
    /// Pico com queda lenta, para a barra não piscar.
    shown_db: f32,
    /// Gravação em andamento (o ffmpeg roda à parte; a janela só acompanha).
    rec: Option<Recorder>,
    rec_video: bool,
    rec_video_format: Format,
    rec_audio_format: Format,
    /// Fonte do som, como em `emeetctl record --mic`: padrao, limpo, nenhum ou a placa.
    rec_audio: String,
    rec_last: Option<PathBuf>,
    rec_error: Option<String>,
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
            preview_on: !std::env::args().any(|a| a == "--no-preview"),
            preview: None,
            texture: None,
            preview_format: None,
            preview_error: None,
            focused: true,
            preview_src: None,
            last_error: None,
            mic_gain: 0,
            mic_gain_edited: None,
            phones: 0.0,
            phones_edited: None,
            meter: None,
            meter_error: None,
            shown_db: mic::FLOOR_DB,
            rec: None,
            rec_video: true,
            rec_video_format: Format::Mp4,
            rec_audio_format: Format::Mp3,
            rec_audio: "padrao".into(),
            rec_last: None,
            rec_error: None,
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
            let editing = self
                .brightness_edited
                .is_some_and(|t| t.elapsed() < EDIT_GRACE);
            if let (Some(p), false) = (l.brightness_percent, editing) {
                self.brightness = p as i32;
            }
            let typing = self
                .hex_edited
                .is_some_and(|t| t.elapsed() < Duration::from_secs(10));
            if let (Some(c), Some(Mode::Ambient), false) = (
                l.color.as_deref().and_then(|h| light::parse_hex(h).ok()),
                l.mode,
                typing,
            ) {
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
        if let Ok(m) = &s.mic {
            if self
                .mic_gain_edited
                .is_none_or(|t| t.elapsed() >= EDIT_GRACE)
            {
                self.mic_gain = m.gain_db.round() as i32;
            }
            if let (Some(h), true) = (
                &m.headphones,
                self.phones_edited.is_none_or(|t| t.elapsed() >= EDIT_GRACE),
            ) {
                self.phones = h.db;
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

    /// Outros programas com a câmera aberta, segundo a última leitura do worker.
    fn in_use_by(&self) -> Option<String> {
        let info = self.snap.as_ref()?.info.as_ref()?;
        emeetctl_core::describe_users(&info.in_use_by)
    }

    fn sync_preview(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if focused && !self.focused {
            // Voltou para a janela: tenta de novo, mesmo que antes estivesse ocupada.
            self.preview_error = None;
        }
        self.focused = focused;
        // Com a câmera virtual ligada a prévia lê da última saída. Havendo mais de
        // uma, as outras ficam para os apps e a prévia não precisa soltar a dela ao
        // perder o foco; com uma só, solta como faria com a câmera física.
        let outputs = self
            .snap
            .as_ref()
            .filter(|s| s.virtualcam.active)
            .map(|s| s.virtualcam.outputs.clone())
            .unwrap_or_default();
        let keep = outputs.len() > 1;
        let virtual_dev = outputs.last().map(|o| o.device.clone());
        let wanted = self.preview_on && (focused || keep);
        let video = virtual_dev.or_else(|| {
            self.snap
                .as_ref()
                .and_then(|s| s.info.as_ref())
                .and_then(|i| i.video.clone())
        });
        if self.preview.is_some() && video != self.preview_src {
            // Trocou a fonte (ligou/desligou a câmera virtual): reabre.
            self.preview = None;
            self.texture = None;
        }
        self.preview_src = video.clone();
        // Sem a câmera virtual, a gravação de vídeo fica com a câmera física.
        let wanted = wanted && self.recording_from().is_none_or(|r| Some(r) != video);
        match (wanted, video, self.preview.is_some()) {
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
                    None => {
                        self.texture =
                            Some(ctx.load_texture("preview", img, egui::TextureOptions::LINEAR))
                    }
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

        match (&self.texture, self.preview_on && self.focused) {
            (Some(t), true) => {
                egui::Image::new(t).corner_radius(14.0).paint_at(ui, rect);
                theme::chip(ui, rect.min + egui::vec2(12.0, 12.0), "AO VIVO", Some(LIVE));
                if let Some(r) = &self.rec {
                    let pos = egui::pos2(rect.left() + 92.0, rect.top() + 12.0);
                    theme::chip(ui, pos, &format!("REC {}", clock(r.elapsed())), Some(LIVE));
                }
                if let Some(f) = &self.preview_format {
                    let pos = egui::pos2(rect.left() + 12.0, rect.bottom() - 36.0);
                    theme::chip(ui, pos, f, None);
                }
            }
            _ => {
                let (msg, color) = match (&self.preview_error, self.preview_on, self.focused) {
                    _ if self.recording_from().is_some_and(|r| Some(r) == self.preview_src) => (
                        "Gravando: sem a câmera virtual, a câmera fica só com a gravação".to_string(),
                        MUTED,
                    ),
                    (Some(e), _, _) => match self.in_use_by() {
                        Some(who) => (format!("A câmera está em uso por {who}"), WARN),
                        None => (format!("Sem prévia: {e}"), WARN),
                    },
                    (None, false, _) => ("Prévia pausada".to_string(), MUTED),
                    (None, true, false) => (
                        match self.in_use_by() {
                            Some(who) => format!("Câmera liberada para outros apps · em uso por {who}"),
                            None => "Câmera liberada para outros apps enquanto o emeetctl está em segundo plano".to_string(),
                        },
                        MUTED,
                    ),
                    (None, true, true) => ("Abrindo a câmera…".to_string(), MUTED),
                };
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    msg,
                    egui::FontId::proportional(15.0),
                    color,
                );
            }
        }

        let label = if self.preview_on {
            "Pausar prévia"
        } else {
            "Mostrar prévia"
        };
        let btn = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 142.0, rect.top() + 10.0),
            egui::vec2(132.0, 30.0),
        );
        if ui
            .put(
                btn,
                egui::Button::new(label).fill(Color32::from_black_alpha(170)),
            )
            .clicked()
        {
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
            if switch_row(
                ui,
                "Luz de preenchimento",
                Some("Fill Light: toda a luz do anel"),
                &mut fill,
            ) {
                self.send(Cmd::Power(Target::Fill, fill));
            }
            ui.separator();
            let mut amb = st.ambient.unwrap_or(false);
            if switch_row(ui, "Anel RGB", Some("Ambient: a cor do anel"), &mut amb) {
                self.send(Cmd::Power(Target::Ambient, amb));
            }
        });

        // Com o anel RGB desligado, cor e arco-íris não têm o que acender: somem da tela.
        let ring_on = st.ambient.unwrap_or(false);
        card(ui, "Modo", |ui| {
            let (modes, labels): (&[Mode], &[&str]) = if ring_on {
                (
                    &[
                        Mode::Cool,
                        Mode::Warm,
                        Mode::Balanced,
                        Mode::Ambient,
                        Mode::Rainbow,
                    ],
                    &["Frio", "Quente", "Neutro", "Cor", "Arco-íris"],
                )
            } else {
                (
                    &[Mode::Cool, Mode::Warm, Mode::Balanced],
                    &["Frio", "Quente", "Neutro"],
                )
            };
            let sel = st.mode.and_then(|m| modes.iter().position(|x| *x == m));
            if let Some(i) = segmented(ui, labels, sel) {
                self.send(Cmd::Mode(modes[i]));
            }
            if !ring_on {
                ui.label(
                    RichText::new("Ligue o anel RGB para escolher cores e o arco-íris.")
                        .small()
                        .color(MUTED),
                );
            }
        });

        if ring_on {
            self.color_card(ui, &st);
        }

        card(ui, "Brilho", |ui| {
            if slider_row(
                ui,
                "Intensidade",
                &mut self.brightness,
                0..=100,
                1,
                true,
                " %",
            ) {
                self.brightness_edited = Some(Instant::now());
                self.send(Cmd::Brightness(self.brightness as u8));
            }
        });
    }

    fn color_card(&mut self, ui: &mut egui::Ui, st: &emeetctl_core::LightStatus) {
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
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.hex)
                        .desired_width(84.0)
                        .char_limit(7)
                        .font(egui::TextStyle::Monospace),
                );
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
                ui.label(
                    RichText::new("Escolher uma cor muda o modo para Cor.")
                        .small()
                        .color(MUTED),
                );
            }
        });
    }

    fn image_tab(&mut self, ui: &mut egui::Ui) {
        let ctrls = self.controls();
        if ctrls.is_empty() {
            let err = self
                .snap
                .as_ref()
                .and_then(|s| s.controls.as_ref().err().cloned());
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
                        ui.label(
                            RichText::new("A câmera ignora o zoom em 1080p, 4K e no formato YUYV.")
                                .small()
                                .color(MUTED),
                        );
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
        if ui
            .add(
                egui::Button::new("Restaurar padrões de fábrica")
                    .min_size(egui::vec2(ui.available_width(), 34.0)),
            )
            .clicked()
        {
            self.ctrl_edited.clear();
            self.send(Cmd::ResetCtrls);
        }
    }

    /// Switch "Automático" e, embaixo, o controle manual que ele destrava.
    fn auto_pair(
        &mut self,
        ui: &mut egui::Ui,
        auto: &Control,
        pair: &AutoPair,
        manual: Option<&Control>,
    ) {
        let v = self.value(auto);
        let (mut on, on_value, off_value) = match &auto.kind {
            // auto_exposure é menu: "Manual Mode" desliga, o outro item liga.
            ControlKind::Menu { items } => {
                let manual_item = items
                    .iter()
                    .find(|(_, n)| n.to_lowercase().contains("manual"))
                    .map(|(i, _)| *i);
                let auto_item = items
                    .iter()
                    .find(|(i, _)| Some(*i) != manual_item)
                    .map(|(i, _)| *i);
                (
                    Some(v) != manual_item,
                    auto_item.unwrap_or(auto.default),
                    manual_item.unwrap_or(1),
                )
            }
            _ => (v != 0, 1, 0),
        };
        if switch_row(ui, "Automático", None, &mut on) {
            self.set_ctrl(auto, if on { on_value } else { off_value });
        }
        if let Some(m) = manual {
            let mut mv = self.value(m);
            if slider_row(
                ui,
                pair.manual_label,
                &mut mv,
                m.min..=m.max,
                m.step,
                !on,
                "",
            ) {
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
            ControlKind::Integer => {
                slider_row(ui, label, &mut v, c.min..=c.max, c.step, enabled, "")
            }
        };
        if changed {
            self.set_ctrl(c, v);
        }
    }

    fn sync_meter(&mut self, ctx: &egui::Context) {
        let node = self
            .snap
            .as_ref()
            .and_then(|s| s.mic.as_ref().ok())
            .and_then(|m| m.pipewire_node.clone())
            .filter(|_| self.tab == Tab::Mic && self.focused);
        if self.meter.as_ref().map(|(n, _)| n) == node.as_ref() {
            return;
        }
        self.meter = None;
        self.shown_db = mic::FLOOR_DB;
        if let Some(n) = node {
            let c = ctx.clone();
            match Meter::start(&n, move || c.request_repaint()) {
                Ok(m) => {
                    self.meter = Some((n, m));
                    self.meter_error = None;
                }
                Err(e) => self.meter_error = Some(e.to_string()),
            }
        }
    }

    fn mic_tab(&mut self, ui: &mut egui::Ui) {
        let st = match self.snap.as_ref().map(|s| &s.mic) {
            Some(Ok(st)) => st.clone(),
            other => {
                let err = other.and_then(|r| r.as_ref().err().cloned());
                return self.unavailable(ui, err);
            }
        };
        let mics = self
            .snap
            .as_ref()
            .map(|s| s.mics.clone())
            .unwrap_or_default();
        if mics.len() > 1 {
            let labels: Vec<String> = mics.iter().map(|m| m.short()).collect();
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            let sel = mics.iter().position(|m| m.card == st.device.card);
            if let Some(i) = segmented(ui, &refs, sel) {
                self.mic_gain_edited = None;
                self.phones_edited = None;
                self.send(Cmd::MicSelect(mics[i].card));
            }
            ui.add_space(4.0);
        }
        card(ui, &st.device.label(), |ui| {
            let mut on = st.on;
            if switch_row(ui, "Ligado", Some("Desligado = mudo"), &mut on) {
                self.send(Cmd::MicOn(on));
            }
            ui.separator();
            if slider_row(
                ui,
                "Ganho",
                &mut self.mic_gain,
                st.min_db.round() as i32..=st.max_db.round() as i32,
                1,
                st.on,
                " dB",
            ) {
                self.mic_gain_edited = Some(Instant::now());
                self.send(Cmd::MicGain(self.mic_gain as f32));
            }
            ui.add_space(6.0);
            self.meter_bar(ui, st.on, st.device.model.mute_button());
        });
        if let Some(h) = &st.headphones {
            card(ui, "Fone", |ui| {
                let mut on = h.on;
                if switch_row(ui, "Ligado", Some("Saída de fone do microfone"), &mut on) {
                    self.send(Cmd::HeadphonesOn(on));
                }
                ui.separator();
                if slider_db(
                    ui,
                    "Volume",
                    &mut self.phones,
                    h.min_db..=h.max_db,
                    0.5,
                    h.on,
                ) {
                    self.phones_edited = Some(Instant::now());
                    self.send(Cmd::Headphones(self.phones));
                }
            });
        }
        let cs = self.snap.as_ref().map(|s| s.clean.clone());
        if let Some(cs) = cs {
            card(ui, "Microfone limpo", |ui| {
                let here = cs.source.is_some() && cs.source == st.pipewire_node;
                let mut on = cs.active && here;
                let hint = if cs.active && !here {
                    "Ligado em outro microfone: ligar aqui troca".to_string()
                } else {
                    format!(
                        "Entrada nova \"{} (limpo)\": ruído, grave e eco do alto-falante tratados pelo WebRTC",
                        st.device.short()
                    )
                };
                if switch_row(ui, "Redução de ruído e eco", Some(&hint), &mut on) {
                    self.send(Cmd::Clean(on));
                }
                if cs.active && here {
                    ui.add_space(4.0);
                    if cs.default {
                        ui.label(
                            RichText::new("Os apps usam a entrada limpa (é a padrão).")
                                .small()
                                .color(MUTED),
                        );
                    } else if ui
                        .add(
                            egui::Button::new("Usar a entrada limpa como padrão")
                                .min_size(egui::vec2(ui.available_width(), 34.0)),
                        )
                        .clicked()
                    {
                        self.send(Cmd::CleanDefault);
                    }
                }
            });
        }
        card(ui, "Sistema", |ui| {
            egui::Grid::new("mic-sys")
                .num_columns(2)
                .spacing([18.0, 10.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Entrada padrão").color(MUTED));
                    ui.label(if st.default { "sim" } else { "não" });
                    ui.end_row();
                    ui.label(RichText::new("Em uso por").color(MUTED));
                    ui.label(if st.in_use_by.is_empty() {
                        "ninguém".to_string()
                    } else {
                        st.in_use_by.join(", ")
                    });
                    ui.end_row();
                });
            if !st.default {
                ui.add_space(6.0);
                if ui
                    .add(
                        egui::Button::new("Usar como entrada padrão")
                            .min_size(egui::vec2(ui.available_width(), 34.0)),
                    )
                    .clicked()
                {
                    self.send(Cmd::MicDefault);
                }
            }
        });
    }

    fn meter_bar(&mut self, ui: &mut egui::Ui, on: bool, mute_button: Option<&str>) {
        let (peak, silence) = self
            .meter
            .as_ref()
            .map_or((mic::FLOOR_DB, false), |(_, m)| {
                (m.peak_db(), m.digital_silence())
            });
        // sobe na hora, desce ~20 dB/s
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        self.shown_db = peak.max(self.shown_db - 20.0 * dt);
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 12.0), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6.0, theme::FIELD);
        let f = mic::fraction(self.shown_db);
        if f > 0.0 && on {
            let mut fill = rect;
            fill.set_width(rect.width() * f);
            let color = match self.shown_db {
                d if d > -3.0 => LIVE,
                d if d > -12.0 => WARN,
                _ => ACCENT,
            };
            painter.rect_filled(fill, 6.0, color);
        }
        let text = match (&self.meter, &self.meter_error, on) {
            (_, _, false) => "mudo".into(),
            (_, Some(e), _) => e.clone(),
            (None, _, _) => "medidor pausado (janela fora de foco)".into(),
            (Some(_), _, _) if self.shown_db <= mic::FLOOR_DB => "silêncio".into(),
            (Some(_), _, _) => format!("{:.1} dBFS · voz entre -18 e -6", self.shown_db),
        };
        ui.label(RichText::new(text).small().color(MUTED));
        if silence && on {
            let why = mute_button
                .unwrap_or("o microfone está mandando zeros: tem mudo no próprio aparelho?");
            ui.label(RichText::new(format!("Sinal zerado: {why}")).color(WARN));
        }
    }

    /// Nó de vídeo que a gravação está lendo.
    fn recording_from(&self) -> Option<String> {
        self.rec
            .as_ref()
            .and_then(|r| r.video())
            .map(|p| p.display().to_string())
    }

    fn rec_format(&self) -> Format {
        if self.rec_video {
            self.rec_video_format
        } else {
            self.rec_audio_format
        }
    }

    fn start_recording(&mut self) {
        let format = self.rec_format();
        let virtual_on = self.snap.as_ref().is_some_and(|s| s.virtualcam.active);
        let mut o = Options::new(format);
        if format.has_video() {
            if virtual_on {
                // a prévia fica na saída dela; a gravação pega outra
                o.avoid = self
                    .preview
                    .as_ref()
                    .and(self.preview_src.as_ref())
                    .map(PathBuf::from);
            } else {
                // só um programa lê a câmera física: a prévia solta para a gravação
                self.preview = None;
                self.texture = None;
            }
        }
        let started = Audio::parse(Some(&self.rec_audio)).and_then(|a| {
            o.audio = a;
            Recorder::start(&o)
        });
        match started {
            Ok(r) => {
                self.rec = Some(r);
                self.rec_error = None;
            }
            Err(e) => self.rec_error = Some(e.to_string()),
        }
    }

    fn stop_recording(&mut self) {
        if let Some(r) = self.rec.take() {
            match r.stop() {
                Ok(p) => self.rec_last = Some(p),
                Err(e) => self.rec_error = Some(e.to_string()),
            }
        }
    }

    /// Gravação que terminou sozinha (tempo, erro, câmera desconectada).
    fn sync_recording(&mut self, ctx: &egui::Context) {
        let Some(r) = self.rec.as_mut() else { return };
        match r.finished() {
            Some(Ok(p)) => self.rec_last = Some(p),
            Some(Err(e)) => self.rec_error = Some(e.to_string()),
            None => {
                ctx.request_repaint_after(Duration::from_millis(250));
                return;
            }
        }
        self.rec = None;
    }

    /// Fontes de som: (rótulo, seleção de `Audio::parse`).
    fn audio_choices(&self) -> Vec<(String, String)> {
        let mut v = vec![(
            "Entrada padrão do sistema".to_string(),
            "padrao".to_string(),
        )];
        if let Some(s) = &self.snap {
            if s.clean.active {
                v.push(("Microfone limpo".into(), "limpo".into()));
            }
            for m in &s.mics {
                v.push((m.short(), m.card.to_string()));
            }
        }
        if self.rec_video {
            v.push(("Sem som".into(), "nenhum".into()));
        }
        v
    }

    fn record_tab(&mut self, ui: &mut egui::Ui) {
        let recording = self.rec.is_some();
        card(ui, "Gravar", |ui| {
            ui.add_enabled_ui(!recording, |ui| {
                if let Some(i) = segmented(
                    ui,
                    &["Vídeo e som", "Só áudio"],
                    Some(!self.rec_video as usize),
                ) {
                    self.rec_video = i == 0;
                    if !self.rec_video && self.rec_audio == "nenhum" {
                        self.rec_audio = "padrao".into();
                    }
                }
                ui.add_space(6.0);
                let formats: &[Format] = if self.rec_video {
                    &Format::VIDEO
                } else {
                    &Format::AUDIO
                };
                let names: Vec<&str> = formats.iter().map(|f| f.name()).collect();
                let cur = self.rec_format();
                if let Some(i) = segmented(ui, &names, formats.iter().position(|f| *f == cur)) {
                    match self.rec_video {
                        true => self.rec_video_format = formats[i],
                        false => self.rec_audio_format = formats[i],
                    }
                }
                ui.label(RichText::new(self.rec_format().hint()).small().color(MUTED));
                ui.add_space(8.0);
                let choices = self.audio_choices();
                let shown = choices
                    .iter()
                    .find(|(_, sel)| *sel == self.rec_audio)
                    .map_or("Entrada padrão do sistema".to_string(), |(l, _)| l.clone());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Som").color(MUTED));
                    egui::ComboBox::from_id_salt("rec-audio")
                        .selected_text(shown)
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for (label, sel) in &choices {
                                ui.selectable_value(&mut self.rec_audio, sel.clone(), label);
                            }
                        });
                });
            });
            ui.add_space(10.0);
            let (text, fill) = match &self.rec {
                Some(r) => (format!("Parar  ·  {}", clock(r.elapsed())), LIVE),
                None => ("Gravar".to_string(), theme::ACCENT_DIM),
            };
            let button = egui::Button::new(RichText::new(text).size(16.0).strong())
                .fill(fill)
                .min_size(egui::vec2(ui.available_width(), 42.0));
            if ui.add(button).clicked() {
                if recording {
                    self.stop_recording();
                } else {
                    self.start_recording();
                }
            }
            ui.add_space(4.0);
            let dir = match &self.rec {
                Some(r) => r.path().display().to_string(),
                None => format!("{}/", record::default_dir(self.rec_video).display()),
            };
            ui.label(
                RichText::new(format!("Salva em {dir}"))
                    .small()
                    .color(MUTED),
            );
            if let Some(e) = &self.rec_error {
                ui.label(RichText::new(e).color(WARN));
            }
            if self.rec_video && !self.snap.as_ref().is_some_and(|s| s.virtualcam.active) {
                ui.label(
                    RichText::new("Sem a câmera virtual, a câmera fica só com a gravação (sem prévia nem Meet ao mesmo tempo).")
                        .small()
                        .color(MUTED),
                );
            }
        });
        if let Some(p) = self.rec_last.clone() {
            card(ui, "Última gravação", |ui| {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string());
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                ui.label(RichText::new(name.unwrap_or_default()).monospace());
                ui.label(
                    RichText::new(format!("{:.1} MB", size as f64 / 1e6))
                        .small()
                        .color(MUTED),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("Abrir").clicked() {
                        open(&p);
                    }
                    if ui.button("Abrir pasta").clicked() {
                        if let Some(dir) = p.parent() {
                            open(dir);
                        }
                    }
                });
            });
        }
    }

    fn device_tab(&self, ui: &mut egui::Ui) {
        let Some(info) = self.snap.as_ref().and_then(|s| s.info.clone()) else {
            return self.unavailable(ui, Some("câmera não encontrada".into()));
        };
        card(ui, "Dispositivo", |ui| {
            egui::Grid::new("info")
                .num_columns(2)
                .spacing([18.0, 10.0])
                .show(ui, |ui| {
                    for (k, v) in [
                        ("Produto", info.product),
                        ("Serial", info.serial),
                        ("Firmware", info.firmware),
                        ("Canal da luz", info.hidraw.unwrap_or("—".into())),
                        ("Vídeo", info.video.unwrap_or("—".into())),
                        (
                            "Em uso por",
                            emeetctl_core::describe_users(&info.in_use_by)
                                .unwrap_or("ninguém".into()),
                        ),
                    ] {
                        ui.label(RichText::new(k).color(MUTED));
                        ui.label(RichText::new(v).monospace());
                        ui.end_row();
                    }
                });
        });
        let vc = self
            .snap
            .as_ref()
            .map(|s| s.virtualcam.clone())
            .unwrap_or_default();
        card(ui, "Câmera virtual", |ui| {
            ui.label(
                RichText::new(
                    "A câmera virtual lê a S600L uma vez e entrega a mesma imagem em várias saídas: \
                     \"emeetctl Camera\", \"emeetctl Camera 2\"... Cada app usa uma saída; a última fica \
                     com esta prévia. Fechar o emeetctl desliga a câmera virtual.",
                )
                .color(MUTED),
            );
            if !vc.installed {
                ui.label(
                    RichText::new("Serviço emeetctl-virtualcam não instalado para este usuário.")
                        .color(WARN),
                );
                return;
            }
            let mut active = vc.active;
            let hint = match (vc.outputs.len(), vc.active) {
                (n, true) if n > 0 => format!("transmitindo em {n} saídas"),
                (_, true) => "iniciando…".to_string(),
                _ => "desligada".to_string(),
            };
            if switch_row(ui, "Ligada", Some(&hint), &mut active) {
                self.send(Cmd::VirtualCam(active));
            }
            if vc.active && !vc.outputs.is_empty() {
                ui.add_space(4.0);
                egui::Grid::new("vcam-outputs")
                    .num_columns(2)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        for o in &vc.outputs {
                            let preview = self.preview.is_some()
                                && self.preview_src.as_deref() == Some(o.device.as_str());
                            let who = match (&o.users, preview) {
                                (Some(u), _) => u.clone(),
                                (None, true) => "esta prévia".to_string(),
                                (None, false) => "livre".to_string(),
                            };
                            ui.label(&o.label).on_hover_text(&o.device);
                            ui.label(RichText::new(who).color(MUTED));
                            ui.end_row();
                        }
                    });
            }
            ui.separator();
            let mut enabled = vc.enabled;
            if switch_row(ui, "Iniciar com a sessão", None, &mut enabled) {
                self.send(Cmd::VirtualCamAutostart(enabled));
            }
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
        let tabs = [Tab::Light, Tab::Image, Tab::Mic, Tab::Record, Tab::Device];
        let sel = tabs.iter().position(|t| *t == self.tab);
        if let Some(i) = segmented(
            ui,
            &["Luz", "Imagem", "Microfone", "Gravar", "Dispositivo"],
            sel,
        ) {
            self.tab = tabs[i];
        }
        ui.add_space(4.0);
        match self.tab {
            Tab::Light => self.light_tab(ui),
            Tab::Image => self.image_tab(ui),
            Tab::Mic => self.mic_tab(ui),
            Tab::Record => self.record_tab(ui),
            Tab::Device => self.device_tab(ui),
        }
    }

    fn header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top")
            .frame(
                egui::Frame::default()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::symmetric(18, 12)),
            )
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
                    let (r, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.5, dot);
                    ui.label(RichText::new(text).color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button("⟳  Atualizar")
                            .on_hover_text("Ler a câmera de novo")
                            .clicked()
                        {
                            self.send(Cmd::Refresh);
                        }
                    });
                });
            });
    }

    fn footer(&mut self, ctx: &egui::Context) {
        let Some((e, t)) = &self.last_error else {
            return;
        };
        if t.elapsed() > Duration::from_secs(8) {
            return;
        }
        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::default()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::symmetric(18, 8)),
            )
            .show(ctx, |ui| {
                ui.label(RichText::new(format!("⚠  {e}")).color(WARN));
            });
        ctx.request_repaint_after(Duration::from_secs(1));
    }
}

/// `--tab luz|imagem|microfone|gravar|dispositivo` abre direto numa aba.
fn initial_tab() -> Tab {
    let args: Vec<String> = std::env::args().collect();
    match args
        .iter()
        .position(|a| a == "--tab")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
    {
        Some("imagem") => Tab::Image,
        Some("microfone") => Tab::Mic,
        Some("gravar") => Tab::Record,
        Some("dispositivo") => Tab::Device,
        _ => Tab::Light,
    }
}

/// 00:00 (ou 1:02:03 depois de uma hora).
fn clock(d: Duration) -> String {
    let t = d.as_secs();
    match t / 3600 {
        0 => format!("{:02}:{:02}", t / 60, t % 60),
        h => format!("{h}:{:02}:{:02}", t / 60 % 60, t % 60),
    }
}

/// Abre o arquivo ou a pasta no programa padrão do sistema.
fn open(p: &Path) {
    if let Ok(mut c) = std::process::Command::new("xdg-open").arg(p).spawn() {
        std::thread::spawn(move || c.wait());
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
        self.sync_recording(ctx);
        self.sync_preview(ctx);
        self.sync_meter(ctx);
        self.header(ctx);
        self.footer(ctx);

        let pad = egui::Margin::same(18);
        if ctx.screen_rect().width() >= WIDE {
            egui::SidePanel::right("side")
                .exact_width(SIDE_WIDTH)
                .resizable(false)
                .frame(egui::Frame::default().fill(theme::PANEL).inner_margin(pad))
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| self.side(ui));
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
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| {
                            let max = egui::vec2(ui.available_width(), 320.0);
                            self.preview_view(ui, max);
                            ui.add_space(6.0);
                            self.side(ui);
                        });
                });
        }
        if self.meter.is_some() {
            // a barra cai devagar mesmo sem amostra nova
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("emeetctl")
            .with_app_id("emeetctl")
            .with_inner_size([1180.0, 720.0])
            .with_min_inner_size([420.0, 560.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!(
                    "../../../assets/icons/hicolor/256x256/apps/emeetctl.png"
                ))
                .expect("ícone embutido é um PNG válido"),
            ),
        ..Default::default()
    };
    let r = eframe::run_native(
        "emeetctl",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    );
    // fechou a janela: a câmera virtual não fica rodando sem o app
    if emeetctl_core::virtualcam::service_active() {
        emeetctl_core::virtualcam::stop_service();
    }
    r
}
