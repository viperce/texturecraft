//! The strip at the top of the window: the app icon at the very left, the menus and the mode label,
//! and the window's title bar unless the system draws one. As the title bar its empty space moves
//! the window and follows the system's double-click setting (craft-window's `title_bar`), and on a
//! window without an OS frame it also has the minimize, maximize/restore and close buttons and
//! invisible resize zones along the window's edges.
//!
//! Who draws the frame is decided once at startup ([`TitleBar::decide`]): Windows draws no OS
//! frame, macOS keeps its traffic lights over the strip (a full-size content view with a hidden
//! title), Linux and the BSDs follow craft-window's probe (the app draws the frame where the
//! compositor leaves it to the app, as GNOME does). `TEXTURECRAFT_TITLEBAR=app` or `=system`
//! overrides that, and Settings › Interface › System title bar brings the OS frame back on the
//! next start. The web build has no window frame: the strip is the menu bar.

use craft_window::{Button, Frame, Layout};
use egui::emath::GuiRounding as _;
use egui::{Color32, CursorIcon, Id, LayerId, Order, PointerButton, Rect, ResizeDirection, Sense, Stroke, Ui, ViewportCommand, pos2, vec2};

use crate::TextureCraftApp;
use crate::i18n::tr;
use crate::theme::Tokens;

/// Who draws the window's title bar.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TitleBar {
    /// The system (or the browser) draws it: the strip is the menu bar.
    #[default]
    System,
    /// macOS: the traffic lights sit over the strip, which keeps room for them at its left.
    Integrated,
    /// No OS frame: the strip is the title bar, with the window buttons in this layout.
    App(Layout),
}

/// One caption button: as wide as Windows' own.
pub const BUTTON_WIDTH: f32 = 46.0;
/// The room at the strip's left for the macOS traffic lights (their width and the gaps around them).
const TRAFFIC_LIGHTS: f32 = 78.0;
/// The app icon's side, in points.
const ICON: f32 = 16.0;
/// The space between the strip's edges (or its caption buttons) and its content.
const PAD: f32 = 8.0;
/// Resize zone thickness along an edge, and the side of a corner zone.
const EDGE: f32 = 5.0;
const CORNER: f32 = 12.0;

/// The app icon's PNGs (`assets/app-icon/hicolor`), smallest first: the strip draws the one that
/// fills [`ICON`] points at the screen's scale without being stretched.
const ICONS: [(f32, &[u8]); 5] = [
    (16.0, include_bytes!("../../../assets/app-icon/hicolor/16x16/apps/ai.storyteller.texturecraft.png")),
    (24.0, include_bytes!("../../../assets/app-icon/hicolor/24x24/apps/ai.storyteller.texturecraft.png")),
    (32.0, include_bytes!("../../../assets/app-icon/hicolor/32x32/apps/ai.storyteller.texturecraft.png")),
    (48.0, include_bytes!("../../../assets/app-icon/hicolor/48x48/apps/ai.storyteller.texturecraft.png")),
    (64.0, include_bytes!("../../../assets/app-icon/hicolor/64x64/apps/ai.storyteller.texturecraft.png")),
];

impl TitleBar {
    /// Decide who draws the title bar; call once at startup, before the window is created.
    /// `system` is Settings › Interface › System title bar.
    pub fn decide(system: bool) -> TitleBar {
        let choice = craft_window::Choice::probe(craft_window::Prefix::new("TEXTURECRAFT"));
        Self::from_choice(choice.frame(), choice.overridden(), system)
    }

    /// `TEXTURECRAFT_TITLEBAR` (`overridden`) wins, then the System title bar setting. Otherwise
    /// Windows draws its own caption, macOS integrates the traffic lights and the other systems
    /// take craft-window's `frame`.
    fn from_choice(frame: &Frame, overridden: Option<bool>, system: bool) -> TitleBar {
        if overridden.is_none() && system {
            return TitleBar::System;
        }
        match frame {
            Frame::App(layout) => TitleBar::App(layout.clone()),
            Frame::Desktop if overridden.is_some() => TitleBar::System,
            Frame::Desktop if cfg!(windows) => TitleBar::App(Layout::default()),
            Frame::Desktop if cfg!(target_os = "macos") => TitleBar::Integrated,
            Frame::Desktop => TitleBar::System,
        }
    }

    /// `builder` with this frame: decorations off for the app's own, and on macOS the content
    /// under a transparent title bar without its title.
    pub fn viewport(&self, builder: egui::ViewportBuilder) -> egui::ViewportBuilder {
        let frame = match self {
            TitleBar::App(layout) => Frame::App(layout.clone()),
            _ => Frame::Desktop,
        };
        let builder = craft_window::decorate(builder, &frame);
        if *self == TitleBar::Integrated { builder.with_fullsize_content_view(true).with_titlebar_shown(false).with_title_shown(false) } else { builder }
    }

    /// The name `ui.inspect` reports: `app`, `integrated` or `system`.
    pub fn name(&self) -> &'static str {
        match self {
            TitleBar::System => "system",
            TitleBar::Integrated => "integrated",
            TitleBar::App(_) => "app",
        }
    }
}

/// The strip's height: the body text with room around it, 28 to 32 points (so the macOS traffic
/// lights and Windows-sized caption buttons fit it).
pub fn height(ui: &Ui) -> f32 {
    (ui.text_style_height(&egui::TextStyle::Body) + 14.0).clamp(28.0, 32.0)
}

/// The strip: menus between the app icon and the mode label, the window buttons at the ends the
/// layout names, and as the title bar its empty space moving the window.
pub fn show(app: &mut TextureCraftApp, ui: &mut Ui) {
    let tk = Tokens::get(ui.ctx());
    let bar = app.title_bar.clone();
    let (left, right) = match &bar {
        TitleBar::App(layout) => (layout.left.as_slice(), layout.right.as_slice()),
        _ => (&[][..], &[][..]),
    };
    egui::Panel::top("menu_bar").exact_size(height(ui)).show_separator_line(false).frame(egui::Frame::NONE.fill(tk.chrome)).show(ui, |ui| {
        let rect = ui.max_rect();
        // First, so the strip's controls drawn over it keep their own clicks.
        if bar != TitleBar::System {
            craft_window::title_bar::empty_area(ui, rect);
        }
        let ends = |n: usize| n as f32 * BUTTON_WIDTH;
        let inset = if bar == TitleBar::Integrated { TRAFFIC_LIGHTS } else { ends(left.len()) + PAD };
        let content = Rect::from_min_max(pos2(rect.left() + inset, rect.top()), pos2(rect.right() - ends(right.len()) - PAD, rect.bottom()));
        ui.scope_builder(egui::UiBuilder::new().max_rect(content).layout(craft_ui::direction::row(ui)), |ui| {
            paint_icon(ui);
            crate::menus::menu_bar(app, ui);
        });
        caption_buttons(ui, left, rect.left(), rect);
        caption_buttons(ui, right, rect.right() - ends(right.len()), rect);
    });
    if matches!(bar, TitleBar::App(_)) {
        resize_zones(ui);
    }
}

/// The app icon at the cursor, [`ICON`] points square, centred on the row and on whole pixels.
fn paint_icon(ui: &mut Ui) {
    let ppp = ui.ctx().pixels_per_point();
    let Some(&(px, png)) = ICONS.iter().find(|(px, _)| *px >= ICON * ppp - 0.5).or(ICONS.last()) else { return };
    let id = Id::unique(("app_icon", px as u32));
    let texture = match ui.data(|d| d.get_temp::<Option<egui::TextureHandle>>(id)) {
        Some(t) => t,
        None => {
            // Decoded once per size; a PNG that doesn't decode leaves the space empty.
            let t = image::load_from_memory(png).ok().map(|img| {
                let img = img.into_rgba8();
                let size = [img.width() as usize, img.height() as usize];
                ui.ctx().load_texture("app_icon", egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), egui::TextureOptions::LINEAR)
            });
            ui.data_mut(|d| d.insert_temp(id, t.clone()));
            t
        }
    };
    let row = ui.max_rect();
    let x = if craft_ui::direction::is_rtl(ui) { ui.cursor().max.x - ICON } else { ui.cursor().min.x };
    let min = pos2(x, row.center().y - ICON / 2.0).round_to_pixels(ppp);
    if let Some(t) = texture {
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        ui.painter().image(t.id(), Rect::from_min_size(min, vec2(ICON, ICON)), uv, Color32::WHITE);
    }
    // The menu titles have no padding of their own: the gap the strip keeps at its edges.
    ui.add_space(ICON + PAD);
}

fn maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false))
}

/// Caption button ids (stable, so tests and agents can find them).
fn caption_id(b: Button) -> Id {
    Id::unique(("titlebar_caption", b as u8))
}

/// `buttons` from `x` rightwards, the strip's full height, flush with its top so a maximized
/// window's corner hits the outermost one.
fn caption_buttons(ui: &mut Ui, buttons: &[Button], x: f32, bar: Rect) {
    let tk = Tokens::get(ui.ctx());
    let max = maximized(ui.ctx());
    let focused = ui.input(|i| i.viewport().focused.unwrap_or(true));
    for (i, &b) in buttons.iter().enumerate() {
        let r = Rect::from_min_size(pos2(x + i as f32 * BUTTON_WIDTH, bar.top()), vec2(BUTTON_WIDTH, bar.height()));
        let resp = ui.interact(r, caption_id(b), Sense::click());
        let tip = match b {
            Button::Minimize => tr("Minimize"),
            Button::Maximize if max => tr("Restore"),
            Button::Maximize => tr("Maximize"),
            Button::Close => tr("Close"),
        };
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tip));
        let close = b == Button::Close;
        let fill = match (resp.is_pointer_button_down_on(), resp.hovered()) {
            (true, _) if close => tk.danger.gamma_multiply(0.8),
            (_, true) if close => tk.danger,
            (true, _) => tk.button,
            (_, true) => tk.row_hover,
            _ => Color32::TRANSPARENT,
        };
        ui.painter().rect_filled(r, 0.0, fill);
        // White on the red, as every system's close button; dimmed while the window is inactive.
        let ink = if close && resp.hovered() {
            Color32::WHITE
        } else if focused {
            tk.text
        } else {
            tk.text_dim
        };
        paint_glyph(ui, b, max, r.center(), ink);
        if resp.on_hover_text(tip).clicked() {
            ui.ctx().send_viewport_cmd(match b {
                Button::Minimize => ViewportCommand::Minimized(true),
                Button::Maximize => ViewportCommand::Maximized(!max),
                // The same request as the system's close button.
                Button::Close => ViewportCommand::Close,
            });
        }
    }
}

/// The 10-point line glyphs, one physical pixel wide per whole step of the screen's scale and on
/// pixel centres, so they stay crisp at any scale.
fn paint_glyph(ui: &Ui, b: Button, maximized: bool, at: egui::Pos2, ink: Color32) {
    let ppp = ui.ctx().pixels_per_point();
    let px = ppp.round().max(1.0);
    let stroke = Stroke::new(px / ppp, ink);
    // An odd width centres the line in a pixel, an even one on a pixel edge.
    let snap = |v: f32| if px as u32 % 2 == 1 { v.round_to_pixel_center(ppp) } else { v.round_to_pixels(ppp) };
    let g = Rect::from_min_max(pos2(snap(at.x - 5.0), snap(at.y - 5.0)), pos2(snap(at.x + 5.0), snap(at.y + 5.0)));
    let p = ui.painter();
    match b {
        Button::Minimize => {
            p.line_segment([g.left_center(), g.right_center()].map(|q| pos2(q.x, snap(q.y))), stroke);
        }
        // Restore: a front square with the corner of the one behind it.
        Button::Maximize if maximized => {
            let front = Rect::from_min_max(pos2(g.left(), snap(g.top() + 2.0)), pos2(snap(g.right() - 2.0), g.bottom()));
            p.rect_stroke(front, 0.0, stroke, egui::StrokeKind::Middle);
            let (l, t) = (snap(g.left() + 2.0), front.top());
            p.line(
                vec![pos2(l, t), pos2(l, g.top()), g.right_top(), pos2(g.right(), snap(g.bottom() - 2.0)), pos2(front.right(), snap(g.bottom() - 2.0))],
                stroke,
            );
        }
        Button::Maximize => {
            p.rect_stroke(g, 0.0, stroke, egui::StrokeKind::Middle);
        }
        Button::Close => {
            p.line_segment([g.left_top(), g.right_bottom()], stroke);
            p.line_segment([g.right_top(), g.left_bottom()], stroke);
        }
    }
}

/// The edge and corner zones of a window with content rect `w`: (zone, direction, cursor). Corners
/// come last so they win where they overlap the edges.
fn zones(w: Rect) -> [(Rect, ResizeDirection, CursorIcon); 8] {
    use ResizeDirection::*;
    let (e, c) = (EDGE, CORNER);
    [
        (Rect::from_min_max(w.left_top(), pos2(w.right(), w.top() + e)), North, CursorIcon::ResizeVertical),
        (Rect::from_min_max(pos2(w.left(), w.bottom() - e), w.right_bottom()), South, CursorIcon::ResizeVertical),
        (Rect::from_min_max(w.left_top(), pos2(w.left() + e, w.bottom())), West, CursorIcon::ResizeHorizontal),
        (Rect::from_min_max(pos2(w.right() - e, w.top()), w.right_bottom()), East, CursorIcon::ResizeHorizontal),
        (Rect::from_min_size(w.left_top(), vec2(c, c)), NorthWest, CursorIcon::ResizeNwSe),
        (Rect::from_min_size(w.right_top() - vec2(c, 0.0), vec2(c, c)), NorthEast, CursorIcon::ResizeNeSw),
        (Rect::from_min_size(w.left_bottom() - vec2(0.0, c), vec2(c, c)), SouthWest, CursorIcon::ResizeNeSw),
        (Rect::from_min_size(w.right_bottom() - vec2(c, c), vec2(c, c)), SouthEast, CursorIcon::ResizeNwSe),
    ]
}

/// Invisible zones along the window's edges, above everything else; a press in one starts the
/// system's resize in that direction. None while the window is maximized or full screen.
fn resize_zones(ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    if maximized(&ctx) || ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) {
        return;
    }
    let w = ctx.content_rect();
    let zui = ui.new_child(egui::UiBuilder::new().layer_id(LayerId::new(Order::Foreground, Id::unique("titlebar_resize"))).max_rect(w));
    for (i, (r, dir, cursor)) in zones(w).into_iter().enumerate() {
        // Drag only (not focusable): Tab does not stop on the zones.
        let resp = zui.interact(r, Id::unique(("titlebar_resize", i)), Sense::DRAG).on_hover_cursor(cursor);
        if resp.drag_started_by(PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
            // The window manager runs the resize and, on X11 and Wayland, keeps the button
            // release: end egui's drag now, or the next press would start nothing.
            ctx.stop_dragging();
            ctx.input_mut(|i| i.pointer = Default::default());
        }
    }
}

/// Once per frame after the UI, on macOS with the integrated strip: the traffic lights centred on
/// the strip, inset from its left as far as from its top, and AppKit's `movable` flag following
/// what is under the pointer (`movable` keeps the value last set).
#[cfg(target_os = "macos")]
pub fn sync_native(bar: &TitleBar, ui: &Ui, frame: &eframe::Frame, movable: &mut Option<bool>) {
    if *bar != TitleBar::Integrated {
        return;
    }
    let h = height(ui);
    // AppKit's buttons are 14 points tall.
    frame.set_traffic_lights_position(ui.ctx(), egui::Rangef::new(0.0, h), (h - 14.0) / 2.0);
    craft_window::title_bar::sync_movable(ui.ctx(), movable);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_override_wins_then_the_setting_then_the_platform() {
        let gnome = Frame::App(Layout::parse("appmenu:close"));
        // TEXTURECRAFT_TITLEBAR=app / =system, whatever the setting.
        for system in [false, true] {
            assert_eq!(TitleBar::from_choice(&gnome, Some(true), system), TitleBar::App(Layout::parse("appmenu:close")));
            assert_eq!(TitleBar::from_choice(&Frame::Desktop, Some(false), system), TitleBar::System);
        }
        // The setting brings the system's frame back.
        assert_eq!(TitleBar::from_choice(&gnome, None, true), TitleBar::System);
        assert_eq!(TitleBar::from_choice(&Frame::Desktop, None, true), TitleBar::System);
        // The platform: a compositor that leaves the frame to the app gets the app's.
        assert_eq!(TitleBar::from_choice(&gnome, None, false), TitleBar::App(Layout::parse("appmenu:close")));
        let desktop = TitleBar::from_choice(&Frame::Desktop, None, false);
        let expected = if cfg!(windows) {
            TitleBar::App(Layout::default())
        } else if cfg!(target_os = "macos") {
            TitleBar::Integrated
        } else {
            TitleBar::System
        };
        assert_eq!(desktop, expected);
    }

    #[test]
    fn only_the_app_frame_turns_decorations_off() {
        let b = TitleBar::App(Layout::default()).viewport(egui::ViewportBuilder::default());
        assert_eq!(b.decorations, Some(false));
        let b = TitleBar::Integrated.viewport(egui::ViewportBuilder::default());
        assert_eq!((b.decorations, b.fullsize_content_view, b.title_shown), (Some(true), Some(true), Some(false)));
        let b = TitleBar::System.viewport(egui::ViewportBuilder::default());
        assert_eq!((b.decorations, b.fullsize_content_view), (Some(true), None));
    }

    #[test]
    fn resize_corners_win_over_edges() {
        let w = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 600.0));
        let at = |p: egui::Pos2| zones(w).into_iter().rev().find(|(r, ..)| r.contains(p)).map(|(_, d, _)| d);
        assert_eq!(at(pos2(2.0, 2.0)), Some(ResizeDirection::NorthWest));
        assert_eq!(at(pos2(500.0, 1.0)), Some(ResizeDirection::North));
        assert_eq!(at(pos2(998.0, 300.0)), Some(ResizeDirection::East));
        assert_eq!(at(pos2(500.0, 300.0)), None);
    }
}
