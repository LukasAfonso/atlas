use eframe::egui::{self, Color32, FontId, Stroke, TextStyle};

pub const INK: Color32 = Color32::from_rgb(32, 36, 31);
pub const MUTED: Color32 = Color32::from_rgb(111, 118, 108);
pub const CANVAS: Color32 = Color32::from_rgb(241, 240, 233);
pub const PAPER: Color32 = Color32::from_rgb(251, 250, 246);
pub const PAPER_STRONG: Color32 = Color32::from_rgb(253, 252, 248);
pub const LINE: Color32 = Color32::from_rgba_unmultiplied_const(45, 51, 43, 33);
pub const SAGE: Color32 = Color32::from_rgb(71, 112, 95);
pub const SAGE_DARK: Color32 = Color32::from_rgb(41, 72, 59);
pub const SAGE_SOFT: Color32 = Color32::from_rgb(220, 232, 224);
pub const AMBER: Color32 = Color32::from_rgb(162, 100, 43);
pub const ERROR: Color32 = Color32::from_rgb(151, 58, 52);
pub const BODY_FONT_SIZE: f32 = 14.0;

pub fn apply(context: &egui::Context) {
    let mut visuals = egui::Visuals::light();
    visuals.override_text_color = Some(INK);
    visuals.panel_fill = CANVAS;
    visuals.window_fill = PAPER;
    visuals.extreme_bg_color = Color32::from_rgb(235, 234, 227);
    visuals.faint_bg_color = Color32::from_rgb(238, 238, 231);
    visuals.code_bg_color = Color32::from_rgb(235, 235, 229);
    visuals.hyperlink_color = SAGE;
    visuals.selection.bg_fill = SAGE;
    visuals.selection.stroke = Stroke::new(1.0, PAPER);
    visuals.widgets.noninteractive.bg_fill = PAPER;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, INK);
    visuals.widgets.inactive.weak_bg_fill = PAPER;
    visuals.widgets.inactive.bg_fill = PAPER;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, INK);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(238, 238, 231);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(238, 238, 231);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, SAGE);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.5, INK);
    visuals.widgets.active.weak_bg_fill = SAGE_SOFT;
    visuals.widgets.active.bg_fill = SAGE_SOFT;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, SAGE);
    visuals.widgets.active.fg_stroke = Stroke::new(1.5, SAGE_DARK);

    context.all_styles_mut(|style| {
        style.visuals = visuals.clone();
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        style.spacing.interact_size.y = 32.0;
        style
            .text_styles
            .insert(TextStyle::Heading, FontId::proportional(22.0));
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(BODY_FONT_SIZE));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(13.0));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(11.0));
    });
}
