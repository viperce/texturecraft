//! X inverts the mask value while a mask is the paint target; Ctrl+X still cuts the layer

use egui_kittest::Harness;
use texturecraft_engine::StackTarget;
use texturecraft_ui_egui::TextureCraftApp;

/// The sample session with a white mask on its selected layer, painting that mask
fn harness() -> Harness<'static, TextureCraftApp> {
    let mut session = texturecraft_engine::sample_session(256, 64).unwrap();
    texturecraft_engine::execute(&mut session, "mask.add.white", &serde_json::Value::Null).unwrap();
    session.target = StackTarget::Mask;
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 960.0)).build_eframe(|cc| TextureCraftApp::new(cc, session));
    h.run_steps(4);
    h
}

fn mask_value(h: &Harness<'_, TextureCraftApp>) -> f32 {
    h.state().session.tool.mask_value
}

#[test]
fn x_inverts_the_mask_value_and_back() {
    let mut h = harness();
    h.state_mut().session.tool.mask_value = 0.25;
    h.key_press(egui::Key::X);
    h.run_steps(3);
    assert_eq!(mask_value(&h), 0.75);
    h.key_press(egui::Key::X);
    h.run_steps(3);
    assert_eq!(mask_value(&h), 0.25);
}

#[test]
fn x_does_nothing_while_painting_layer_content() {
    let mut h = harness();
    h.state_mut().session.target = StackTarget::Content;
    h.state_mut().session.tool.mask_value = 0.25;
    h.key_press(egui::Key::X);
    h.run_steps(3);
    assert_eq!(mask_value(&h), 0.25);
}

#[test]
fn ctrl_x_still_cuts_the_layer_while_a_mask_is_the_target() {
    let mut h = harness();
    let layer = h.state().session.selected_layer().unwrap();
    h.event(egui::Event::Cut);
    h.run_steps(3);
    assert!(!h.state().session.doc.project.layers.contains_key(&layer), "Ctrl+X removes the layer");
}

#[test]
fn ctrl_x_is_not_x() {
    let mut h = harness();
    // With cut unbound, nothing else may answer Ctrl+X
    h.state_mut().ui.keymap.insert("layer.cut".into(), String::new());
    h.state_mut().session.tool.mask_value = 0.25;
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::X);
    h.run_steps(3);
    assert_eq!(mask_value(&h), 0.25);
}
