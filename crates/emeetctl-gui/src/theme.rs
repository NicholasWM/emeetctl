//! Visual do emeetctl: paleta escura, cartões e os widgets que o egui não traz prontos
//! (switch, controle segmentado, amostra de cor redonda, linha de slider larga).

use eframe::egui::{
    self, Color32, CornerRadius, FontId, Margin, Response, RichText, Sense, Stroke, StrokeKind, TextStyle, Ui,
};

pub const BG: Color32 = Color32::from_rgb(0x12, 0x14, 0x17);
pub const PANEL: Color32 = Color32::from_rgb(0x18, 0x1b, 0x1f);
pub const CARD: Color32 = Color32::from_rgb(0x20, 0x24, 0x29);
pub const CARD_EDGE: Color32 = Color32::from_rgb(0x2b, 0x30, 0x36);
pub const FIELD: Color32 = Color32::from_rgb(0x2c, 0x31, 0x38);
pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe8, 0xeb);
pub const MUTED: Color32 = Color32::from_rgb(0x8b, 0x93, 0x9c);
pub const ACCENT: Color32 = Color32::from_rgb(0x3d, 0xd6, 0xb5);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x1f, 0x5c, 0x50);
pub const LIVE: Color32 = Color32::from_rgb(0xff, 0x5a, 0x5f);
pub const WARN: Color32 = Color32::from_rgb(0xf0, 0x9a, 0x5a);

pub fn apply(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = BG;
    v.faint_bg_color = CARD;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    v.slider_trailing_fill = true;
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
        w.corner_radius = CornerRadius::same(8);
    }
    v.widgets.inactive.bg_fill = FIELD;
    v.widgets.inactive.weak_bg_fill = FIELD;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x36, 0x3c, 0x44);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x36, 0x3c, 0x44);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, CARD_EDGE);
    v.widgets.active.bg_fill = ACCENT_DIM;
    v.widgets.active.weak_bg_fill = ACCENT_DIM;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, CARD_EDGE);
    v.window_corner_radius = CornerRadius::same(12);
    ctx.set_visuals(v);

    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(10.0, 10.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.interact_size.y = 26.0;
        s.text_styles.insert(TextStyle::Body, FontId::proportional(14.5));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
        s.text_styles.insert(TextStyle::Heading, FontId::proportional(20.0));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(13.0));
    });
}

/// Cartão com título em caixa-alta discreta.
pub fn card<R>(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::default()
        .fill(CARD)
        .stroke(Stroke::new(1.0, CARD_EDGE))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title.to_uppercase()).size(11.5).color(MUTED).strong());
            ui.add_space(4.0);
            add(ui)
        })
        .inner
}

/// Interruptor deslizante.
pub fn switch(ui: &mut Ui, on: &mut bool) -> Response {
    let size = egui::vec2(42.0, 24.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(resp.id, *on);
        let track = lerp_color(FIELD, ACCENT, t);
        let radius = rect.height() / 2.0;
        ui.painter().rect(rect, radius, track, Stroke::NONE, StrokeKind::Inside);
        let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
        let knob = if resp.hovered() { Color32::WHITE } else { Color32::from_gray(235) };
        ui.painter().circle_filled(egui::pos2(x, rect.center().y), radius - 3.5, knob);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Linha "rótulo ........ switch".
pub fn switch_row(ui: &mut Ui, label: &str, hint: Option<&str>, on: &mut bool) -> bool {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(label);
            if let Some(h) = hint {
                ui.label(RichText::new(h).small().color(MUTED));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| switch(ui, on).changed())
            .inner
    })
    .inner
}

/// Botões lado a lado com a mesma largura; devolve o índice clicado.
pub fn segmented(ui: &mut Ui, labels: &[&str], selected: Option<usize>) -> Option<usize> {
    let gap = 4.0;
    let w = (ui.available_width() - gap * (labels.len() as f32 - 1.0)) / labels.len() as f32;
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        ui.spacing_mut().button_padding = egui::vec2(2.0, 6.0);
        for (i, l) in labels.iter().enumerate() {
            let on = selected == Some(i);
            let text = RichText::new(*l).color(if on { BG } else { TEXT });
            let b = egui::Button::new(text)
                .fill(if on { ACCENT } else { FIELD })
                .corner_radius(CornerRadius::same(8))
                .truncate();
            if ui.add_sized([w, 34.0], b).clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// Amostra de cor redonda; anel de destaque quando é a cor atual.
pub fn swatch(ui: &mut Ui, rgb: [u8; 3], selected: bool, hint: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(34.0, 34.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let c = rect.center();
        if selected {
            ui.painter().circle_stroke(c, 16.0, Stroke::new(2.0, TEXT));
        } else if resp.hovered() {
            ui.painter().circle_stroke(c, 16.0, Stroke::new(1.5, MUTED));
        }
        ui.painter().circle_filled(c, 12.5, Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
    }
    resp.on_hover_text(hint).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Rótulo em cima, slider ocupando a largura toda e o valor à direita.
pub fn slider_row(ui: &mut Ui, label: &str, value: &mut i32, range: std::ops::RangeInclusive<i32>, step: i32, enabled: bool, suffix: &str) -> bool {
    ui.label(RichText::new(label).color(if enabled { TEXT } else { MUTED }));
    ui.add_enabled_ui(enabled, |ui| {
        ui.spacing_mut().slider_width = (ui.available_width() - 76.0).max(80.0);
        ui.add(egui::Slider::new(value, range).step_by(step.max(1) as f64).suffix(suffix))
            .changed()
    })
    .inner
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Pílula de texto sobre a prévia.
pub fn chip(ui: &mut Ui, pos: egui::Pos2, text: &str, dot: Option<Color32>) {
    let font = FontId::proportional(12.0);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, TEXT);
    let pad = egui::vec2(10.0, 5.0);
    let dot_w = if dot.is_some() { 14.0 } else { 0.0 };
    let rect = egui::Rect::from_min_size(pos, galley.size() + pad * 2.0 + egui::vec2(dot_w, 0.0));
    ui.painter().rect_filled(rect, 999.0, Color32::from_black_alpha(170));
    if let Some(c) = dot {
        ui.painter().circle_filled(egui::pos2(rect.left() + pad.x + 4.0, rect.center().y), 4.0, c);
    }
    ui.painter().galley(rect.min + pad + egui::vec2(dot_w, 0.0), galley, TEXT);
}
