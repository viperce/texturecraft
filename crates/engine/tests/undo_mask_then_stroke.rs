//! Issue #307: undoing "Add mask" left the paint target on the mask, so the next stroke was
//! recorded as a mask stroke and created a new black mask.
use serde_json::{Value, json};
use texturecraft_engine::geom::glam::Vec2;
use texturecraft_engine::geom::primitives;
use texturecraft_engine::{Session, StackTarget, execute};

/// A small project with one fill layer and a selected paint layer, and a paint brush ready.
fn setup() -> Session {
    let mut s = Session::new_project("t", primitives::sphere(1.0, 48, 24), "builtin:random", None, 256).unwrap();
    s.viewport = Vec2::new(400.0, 400.0);
    execute(&mut s, "layer.add.fill", &json!({"color": [0.2, 0.5, 0.3], "roughness": 0.7})).unwrap();
    execute(&mut s, "layer.add.paint", &Value::Null).unwrap();
    s.update();
    execute(&mut s, "tool.brush", &json!({"size": 20, "hardness": 50, "opacity": 0.8})).unwrap();
    execute(&mut s, "tool.paint", &Value::Null).unwrap();
    execute(&mut s, "tool.material", &json!({"channel": "baseColor", "color": [0.9, 0.3, 0.9]})).unwrap();
    s
}

/// Paint one stroke from `a` to `b` (screen pixels) and let the session settle.
fn stroke(s: &mut Session, a: [i32; 2], b: [i32; 2]) {
    execute(s, "tool.stroke", &json!({"points": [a, b]})).unwrap();
    s.update();
}

/// Run a command with no parameters and return its result; the test fails if the command errors.
fn run(s: &mut Session, cmd: &str) -> Value {
    let v = execute(s, cmd, &Value::Null).unwrap();
    s.update();
    v
}

/// Whether the selected layer currently has a mask.
fn has_mask(s: &Session) -> bool {
    let id = s.selected_layer_id().unwrap();
    s.doc.project.layers.get(&id).unwrap().mask.is_some()
}

/// After undoing Add mask, the next stroke is a normal paint stroke, no mask is recreated,
/// and the settled composite equals a full recomposite.
#[test]
fn a_stroke_after_undoing_a_mask_is_a_paint_stroke_and_matches_a_full_recomposite() {
    let mut s = setup();
    stroke(&mut s, [130, 130], [170, 150]);
    run(&mut s, "mask.add.white");
    run(&mut s, "edit.undo"); // removes the mask again
    assert_eq!(s.target, StackTarget::Content);
    stroke(&mut s, [167, 183], [207, 203]);
    assert!(!has_mask(&s), "the stroke recreated the mask");

    // The stroke should be a normal paint stroke on the layer.
    let undone = run(&mut s, "edit.undo");
    assert_eq!(undone["undone"], "Paint stroke", "the stroke after undoing the mask was recorded as {}", undone["undone"]);
    run(&mut s, "edit.redo");

    let settled = s.caches[0].composite.clone();
    s.caches[0].invalidate_all(); // recomposite every tile
    s.update();
    assert!(settled == s.caches[0].composite, "the composite after the stroke differs from a full recomposite");
}

/// Undo then redo of Add mask: the next stroke goes into the mask again.
#[test]
fn redoing_add_mask_paints_into_the_mask_again() {
    let mut s = setup();
    stroke(&mut s, [130, 130], [170, 150]);
    run(&mut s, "mask.add.white");
    run(&mut s, "edit.undo");
    run(&mut s, "edit.redo");
    assert!(has_mask(&s));
    assert_eq!(s.target, StackTarget::Mask);
    stroke(&mut s, [167, 183], [207, 203]);
    assert_eq!(s.history_labels().last().map(String::as_str), Some("Mask stroke"));
    let id = s.selected_layer_id().unwrap();
    let mask = s.doc.project.layers.get(&id).unwrap().mask.as_ref().unwrap();
    assert_eq!(mask.effects.len(), 1, "the stroke should paint into the mask's paint effect");
}

/// Undoing the mask's paint effect and then the mask leaves the layer paintable as content.
#[test]
fn undoing_a_paint_effect_then_stroking_paints_the_layer() {
    let mut s = setup();
    run(&mut s, "mask.add.white");
    execute(&mut s, "effect.add.paint", &json!({"target": "mask"})).unwrap();
    s.update();
    run(&mut s, "edit.undo"); // the paint effect
    run(&mut s, "edit.undo"); // the mask
    assert_eq!(s.target, StackTarget::Content);
    stroke(&mut s, [130, 130], [170, 150]);
    assert_eq!(s.history_labels().last().map(String::as_str), Some("Paint stroke"));
    assert!(!has_mask(&s));
}

/// Control: with no mask involved, every stroke is a paint stroke.
#[test]
fn two_strokes_without_a_mask_are_both_paint_strokes() {
    let mut s = setup();
    stroke(&mut s, [130, 130], [170, 150]);
    stroke(&mut s, [167, 183], [207, 203]);
    let labels = s.history_labels();
    assert_eq!(&labels[labels.len() - 2..], ["Paint stroke", "Paint stroke"]);
}
