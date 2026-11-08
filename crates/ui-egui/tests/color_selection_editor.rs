//! The color selection editor in Properties: one row per ID color, remove, and the pick mode

use egui::{Event, Modifiers, PointerButton, Pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::{Value, json};
use texturecraft_engine::core::{BakedMap, EffectKind, LayerId};
use texturecraft_engine::{StackTarget, ViewMode, execute};
use texturecraft_ui_egui::TextureCraftApp;

const FIRST: [f32; 3] = [200.0 / 255.0, 0.0, 0.0];
const SECOND: [f32; 3] = [10.0 / 255.0, 1.0, 0.0];
const THIRD: [f32; 3] = [30.0 / 255.0, 0.0, 1.0];
const ID_PART: [u8; 3] = [90, 160, 220];

/// An ID map of one color, as a PNG
fn id_map_png() -> Vec<u8> {
    let [red, green, blue] = ID_PART;
    let image = image::RgbaImage::from_pixel(4, 4, image::Rgba([red, green, blue, 255]));
    let mut bytes = Vec::new();
    image.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
    bytes
}

/// The app with a selected color selection of three colors on a fill layer's mask
fn app_with_color_selection(with_id_map: bool) -> (Harness<'static, TextureCraftApp>, LayerId) {
    let mut session = texturecraft_engine::sample_session(256, 64).unwrap();
    if with_id_map {
        let image = session.import_resource_bytes("id.png", id_map_png()).unwrap();
        execute(&mut session, "textureSet.meshMap.set", &json!({"map": "id", "image": image})).unwrap();
    } else {
        // The sample bakes an ID map with the others
        execute(&mut session, "textureSet.meshMap.clear", &json!({"map": "id"})).unwrap();
    }

    let layer = LayerId(execute(&mut session, "layer.add.fill", &Value::Null).unwrap()["layer"].as_u64().unwrap());
    execute(&mut session, "mask.add.colorSelection", &Value::Null).unwrap();
    let effect = session.doc.project.layer(layer).unwrap().mask.as_ref().unwrap().effects[0].id;
    execute(&mut session, "effect.set", &json!({"effect": effect.0, "colors": [FIRST, SECOND, THIRD]})).unwrap();
    session.target = StackTarget::Effect(effect);

    let mut harness = Harness::builder().with_size(egui::vec2(1600.0, 1400.0)).build_eframe(|cc| TextureCraftApp::new(cc, session));
    harness.run_steps(4);
    (harness, effect)
}

fn colors(harness: &Harness<'_, TextureCraftApp>, effect: LayerId) -> Vec<[f32; 3]> {
    match &harness.state().session.doc.project.effect(effect).unwrap().1.kind {
        EffectKind::ColorSelection { colors, .. } => colors.clone(),
        other => panic!("not a color selection: {other:?}"),
    }
}

fn click_at(harness: &mut Harness<'_, TextureCraftApp>, position: Pos2) {
    harness.event(Event::PointerMoved(position));
    harness.run_steps(1);
    harness.event(Event::PointerButton { pos: position, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    harness.run_steps(1);
    harness.event(Event::PointerButton { pos: position, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    harness.run_steps(3);
}

/// Whether the last frame drew the eyedropper glyph with its tip at `pointer`
fn eyedropper_at(harness: &Harness<'_, TextureCraftApp>, pointer: Pos2) -> bool {
    harness.output().shapes.iter().any(|clipped| match &clipped.shape {
        egui::Shape::Text(text) => text.galley.text() == egui_phosphor::regular::EYEDROPPER && (text.pos.x - pointer.x).abs() < 1.0,
        _ => false,
    })
}

fn pick_color_label() -> String {
    format!("{} Pick color", egui_phosphor::regular::EYEDROPPER)
}

#[test]
fn each_color_gets_a_row_and_remove_sends_the_list_without_it() {
    let (mut harness, effect) = app_with_color_selection(false);
    for readout in ["R: 200", "R: 10", "R: 30"] {
        assert!(harness.query_by_label(readout).is_some(), "{readout} row");
    }

    let second_row = harness.get_by_label("R: 10").rect().center().y;
    let remove = harness
        .query_all_by_label("Remove")
        .find(|button| (button.rect().center().y - second_row).abs() < 8.0)
        .expect("the second row's remove button")
        .rect()
        .center();
    click_at(&mut harness, remove);

    assert_eq!(colors(&harness, effect), vec![FIRST, THIRD]);
}

#[test]
fn pick_mode_shows_the_id_map_and_a_click_on_the_mesh_adds_its_color() {
    let (mut harness, effect) = app_with_color_selection(true);
    harness.get_by_label(&pick_color_label()).click();
    harness.run_steps(3);
    assert!(harness.state().picking_id_color.is_some_and(|pick| pick.effect == effect && pick.index.is_none()));
    assert_eq!(harness.state().session.view, ViewMode::MeshMap(BakedMap::Id));

    let center = harness.state().viewport_rect.center();
    harness.event(Event::PointerMoved(center));
    harness.run_steps(2);
    assert!(eyedropper_at(&harness, center), "the pointer is an eyedropper");

    click_at(&mut harness, center);

    let picked = colors(&harness, effect);
    assert_eq!(picked.len(), 4, "the click adds a row");
    assert_eq!(picked[3], ID_PART.map(|channel| channel as f32 / 255.0));
    assert!(harness.state().picking_id_color.is_none(), "one click ends pick mode");
    assert_eq!(harness.state().session.view, ViewMode::Material, "the earlier view comes back");
}

#[test]
fn without_an_id_map_picking_is_off_and_a_warning_says_why() {
    let (mut harness, _) = app_with_color_selection(false);
    let warning = format!("{} Bake an ID map (F8) to pick colors from the mesh.", egui_phosphor::regular::WARNING);
    assert!(harness.query_by_label(&warning).is_some());

    harness.get_by_label(&pick_color_label()).click();
    harness.run_steps(3);
    assert!(harness.state().picking_id_color.is_none());
}
