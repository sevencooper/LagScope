//! The window's look: egui visuals, fonts and spacing from the shared design tokens
//! (crate::theme, lagscope-core's design/tokens.json), so the app matches LagScope Pulse.

use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle};
use eframe::epaint::text::{FontTweak, VariationCoords};

use crate::theme::{self as t, Color};

pub fn c(x: Color) -> Color32 {
    Color32::from_rgb(x.0[0], x.0[1], x.0[2])
}

/// Space Grotesk: headings, the verdict, big numbers.
pub fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("display".into()))
}

/// JetBrains Mono: numbers and times.
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

fn radius(r: f32) -> CornerRadius {
    CornerRadius::same(r.min(255.0) as u8)
}

/// Fonts, colours, radii and spacing for the whole window.
pub fn apply(ctx: &egui::Context) {
    let mut f = FontDefinitions::default();
    let mut family = |fam: FontFamily, name: &str, files: [&'static [u8]; 2], weight: f32| {
        let names: Vec<String> = (0..files.len()).map(|i| format!("{name}-{i}")).collect();
        for (n, data) in names.iter().zip(files) {
            let tweak = FontTweak { coords: VariationCoords::new([(b"wght", weight)]), ..Default::default() };
            f.font_data.insert(n.clone(), Arc::new(FontData::from_static(data).tweak(tweak)));
        }
        let list = f.families.entry(fam).or_default();
        list.splice(0..0, names);
    };
    family(FontFamily::Proportional, t::FONT_TEXT, t::fonts::TEXT, 400.0);
    family(FontFamily::Monospace, t::FONT_MONO, t::fonts::MONO, 400.0);
    family(FontFamily::Name("display".into()), t::FONT_DISPLAY, t::fonts::DISPLAY, 600.0);
    // Characters outside the display font fall back to the text font, then egui's own.
    let fallback = f.families[&FontFamily::Proportional].clone();
    f.families.get_mut(&FontFamily::Name("display".into())).unwrap().extend(fallback);
    ctx.set_fonts(f);

    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(11.0)),
            (TextStyle::Body, FontId::proportional(13.5)),
            (TextStyle::Button, FontId::proportional(13.5)),
            (TextStyle::Monospace, mono(12.5)),
            (TextStyle::Heading, display(22.0)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 7.0);
        s.spacing.button_padding = egui::vec2(14.0, 5.0);
        s.spacing.interact_size.y = 26.0;
        s.spacing.menu_margin = egui::Margin::same(8);

        let v = &mut s.visuals;
        let pill = radius(t::RADIUS_BUTTON);
        v.panel_fill = c(t::GROUND);
        v.window_fill = c(t::SURFACE);
        v.window_stroke = Stroke::new(1.0, c(t::LINE));
        v.window_corner_radius = radius(t::RADIUS_CARD);
        v.menu_corner_radius = radius(12.0);
        v.extreme_bg_color = c(t::SURFACE);
        v.faint_bg_color = c(t::SURFACE);
        v.code_bg_color = c(t::RAISED);
        v.hyperlink_color = c(t::MINT);
        v.warn_fg_color = c(t::ORANGE);
        v.error_fg_color = c(t::ORANGE);
        v.weak_text_color = Some(c(t::MUTED));
        v.selection.bg_fill = c(t::MINT_BG);
        v.selection.stroke = Stroke::new(1.0, c(t::MINT));

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = c(t::SURFACE);
        w.noninteractive.weak_bg_fill = c(t::SURFACE);
        w.noninteractive.bg_stroke = Stroke::new(1.0, c(t::LINE));
        w.noninteractive.fg_stroke = Stroke::new(1.0, c(t::TEXT));
        for (x, fill, line) in [
            (&mut w.inactive, t::RAISED, t::RAISED_LINE),
            (&mut w.hovered, t::RAISED_LINE, t::MUTED),
            (&mut w.active, t::MINT_BG, t::MINT),
            (&mut w.open, t::RAISED, t::RAISED_LINE),
        ] {
            x.bg_fill = c(fill);
            x.weak_bg_fill = c(fill);
            x.bg_stroke = Stroke::new(1.0, c(line));
            x.fg_stroke = Stroke::new(1.0, c(t::TEXT));
            x.corner_radius = pill;
        }
    });
}

/// A card: surface, thin border, large radius.
pub fn card() -> egui::Frame {
    egui::Frame::new().fill(c(t::SURFACE)).stroke(Stroke::new(1.0, c(t::LINE))).corner_radius(radius(t::RADIUS_CARD)).inner_margin(egui::Margin::symmetric(18, 14))
}

/// The verdict card: green-tinted when nothing was found, warm otherwise.
pub fn verdict(ok: bool) -> egui::Frame {
    let (fill, line) = if ok { (t::VERDICT_OK_BG, t::VERDICT_OK_LINE) } else { (t::VERDICT_WARN_BG, t::VERDICT_WARN_LINE) };
    card().fill(c(fill)).stroke(Stroke::new(1.0, c(line)))
}

/// The panels' frame: ground colour with room around the content.
pub fn panel(top: bool) -> egui::Frame {
    egui::Frame::new().fill(c(t::GROUND)).inner_margin(if top { egui::Margin::symmetric(18, 8) } else { egui::Margin::symmetric(18, 12) })
}

/// A status mark: a round dot for "fine", a square for "worth a look" and "not measured".
pub fn status_mark(ui: &mut egui::Ui, status: &str) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    let (color, round) = t::status(status);
    if round {
        ui.painter().circle_filled(r.center(), 4.5, c(color));
    } else {
        ui.painter().rect_filled(egui::Rect::from_center_size(r.center(), egui::vec2(8.5, 8.5)), 1.5, c(color));
    }
}
