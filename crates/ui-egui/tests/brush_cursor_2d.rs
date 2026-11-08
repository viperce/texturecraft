//! The 2D view hides the system pointer and draws the brush outline under it while painting.

use egui::{CursorIcon, Event, Pos2, Shape, TouchDeviceId, TouchId, TouchPhase};
use egui_kittest::Harness;
use texturecraft_ui_egui::TextureCraftApp;
use texturecraft_ui_egui::state::{Mode, ViewLayout};

fn harness() -> Harness<'static, TextureCraftApp> {
    let session = texturecraft_engine::sample_session(256, 64).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 960.0)).build_eframe(|cc| TextureCraftApp::new(cc, session));
    h.state_mut().ui.layout = ViewLayout::Only2D;
    h.run_steps(4);
    h
}

fn cursor(h: &Harness<'_, TextureCraftApp>) -> CursorIcon {
    h.output().platform_output.cursor_icon
}

#[test]
fn painting_in_the_2d_view_replaces_the_pointer_with_the_brush_outline() {
    let mut h = harness();
    let c = egui::pos2(800.0, 480.0);
    h.hover_at(c);
    h.run_steps(3);
    assert_eq!(cursor(&h), CursorIcon::None, "the brush outline replaces the pointer");
    h.hover_at(egui::pos2(2.0, 2.0));
    h.run_steps(3);
    assert_ne!(cursor(&h), CursorIcon::None, "outside the view the pointer is back");
}

#[test]
fn outside_painting_the_2d_view_keeps_the_normal_pointer() {
    let mut h = harness();
    h.state_mut().ui.mode = Mode::Baking;
    h.run_steps(3);
    let c = egui::pos2(800.0, 480.0);
    h.hover_at(c);
    h.run_steps(3);
    assert_ne!(cursor(&h), CursorIcon::None);
}

/// Radii of the white brush-outline rings (stroke 1.2) around `c` in the last frame.
fn rings_at(h: &Harness<'_, TextureCraftApp>, c: Pos2) -> Vec<f32> {
    fn walk(shape: &Shape, c: Pos2, out: &mut Vec<f32>) {
        match shape {
            Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, c, out)),
            Shape::Circle(circle) if (circle.stroke.width - 1.2).abs() < 1e-3 && circle.center.distance(c) < 0.5 => out.push(circle.radius),
            _ => {}
        }
    }
    let mut out = Vec::new();
    h.output().shapes.iter().for_each(|clipped| walk(&clipped.shape, c, &mut out));
    out
}

#[test]
fn the_view_header_keeps_the_normal_pointer_and_gets_no_brush_outline() {
    let mut h = harness();
    // The view's top edge is where the brush outline first replaces the pointer; the header is
    // centred 8 points below it and 22 high.
    let view_top = (0..200).map(|y| y as f32).find(|&y| {
        h.hover_at(egui::pos2(300.0, y));
        h.run_steps(2);
        cursor(&h) == CursorIcon::None
    });
    let top = view_top.expect("the view has a top edge");
    let header = egui::pos2(800.0, top + 8.0 + 11.0);
    h.hover_at(header);
    h.run_steps(3);
    assert_ne!(cursor(&h), CursorIcon::None, "over the pickers the pointer stays");
    assert!(rings_at(&h, header).is_empty(), "no brush outline over the pickers");
    let below = egui::pos2(800.0, top + 120.0);
    h.hover_at(below);
    h.run_steps(3);
    assert_eq!(cursor(&h), CursorIcon::None, "below the header the brush outline is back");
    assert!(!rings_at(&h, below).is_empty());
}

#[test]
fn a_light_pen_shows_the_smaller_footprint_it_paints() {
    let mut h = harness();
    h.state_mut().session.tool.brush.size = 0.05;
    h.state_mut().session.tool.brush.min_size = 0.5;
    let c = egui::pos2(800.0, 600.0);
    h.hover_at(c);
    h.run_steps(3);
    let full = rings_at(&h, c);
    assert_eq!(full.len(), 1, "one outline at full pressure");
    // A pen sample at half pressure: stamp radius is min + (1 - min) * p = 0.75 of full.
    let events = &mut h.input_mut().events;
    events.push(Event::Touch { device_id: TouchDeviceId(1), id: TouchId(1), phase: TouchPhase::Move, pos: c, force: Some(0.5) });
    events.push(Event::PointerMoved(c));
    h.step();
    let light = rings_at(&h, c);
    assert_eq!(light.len(), 1);
    assert!((light[0] / full[0] - 0.75).abs() < 1e-3, "half pressure shows 0.75 of the radius: {} vs {}", light[0], full[0]);
}

#[test]
fn dragging_out_of_the_2d_view_brings_the_system_pointer_back() {
    let mut h = harness();
    let c = egui::pos2(800.0, 480.0);
    h.hover_at(c);
    h.run_steps(3);
    assert_eq!(cursor(&h), CursorIcon::None, "inside the view the brush outline replaces the pointer");
    // Press inside the view, then carry the pointer out of the view (top-left corner) with the button held.
    h.event(Event::PointerButton { pos: c, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.run_steps(2);
    assert_eq!(cursor(&h), CursorIcon::None, "still inside while dragging");
    let outside = egui::pos2(2.0, 2.0);
    h.hover_at(outside);
    h.run_steps(3);
    assert_ne!(cursor(&h), CursorIcon::None, "outside the view the pointer is visible even during a drag");
    assert!(rings_at(&h, outside).is_empty(), "no brush outline outside the view");
    h.event(Event::PointerButton { pos: outside, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    h.run_steps(2);
}

#[test]
fn a_ctrl_right_drag_out_of_the_2d_view_keeps_its_outline_at_the_anchor() {
    let mut h = harness();
    let c = egui::pos2(800.0, 480.0);
    let ctrl = egui::Modifiers::COMMAND;
    h.hover_at(c);
    h.run_steps(2);
    h.event(Event::ModifiersChanged(ctrl));
    h.event(Event::PointerButton { pos: c, button: egui::PointerButton::Secondary, pressed: true, modifiers: ctrl });
    h.run_steps(2);
    // Drag a little inside the view: the outline is anchored and the pointer hidden.
    h.hover_at(c + egui::vec2(10.0, 0.0));
    h.run_steps(3);
    assert_eq!(rings_at(&h, c).len(), 1, "anchored outline while the pointer is in the view");
    assert_eq!(cursor(&h), CursorIcon::None);
    // Carry the pointer out of the view: the outline stays at the anchor, the system pointer is back.
    h.hover_at(egui::pos2(2.0, 2.0));
    h.run_steps(3);
    assert_eq!(rings_at(&h, c).len(), 1, "the outline stays at the press origin outside the view");
    assert_ne!(cursor(&h), CursorIcon::None, "outside the view the pointer is visible");
    h.event(Event::PointerButton { pos: egui::pos2(2.0, 2.0), button: egui::PointerButton::Secondary, pressed: false, modifiers: ctrl });
    h.event(Event::ModifiersChanged(egui::Modifiers::NONE));
    h.run_steps(2);
}
