//! New project ▸ Import baked maps: added files are listed, Clear empties the list, Create imports them

use std::path::PathBuf;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use texturecraft_engine::core::{BakedMap, NormalFormat};
use texturecraft_ui_egui::TextureCraftApp;
use texturecraft_ui_egui::new_project_advanced::add_baked_map_files;

fn harness() -> Harness<'static, TextureCraftApp> {
    let session = texturecraft_engine::sample_session(256, 64).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 960.0)).build_eframe(|cc| TextureCraftApp::new(cc, session));
    h.state_mut().ui.windows.new_project = true;
    h.state_mut().ui.new_project.resolution = 256;
    h.run_steps(3);
    h.get_by_label("Advanced settings").click();
    h.run_steps(3);
    h.get_by_label("Import baked maps").click();
    h.run_steps(3);
    h
}

/// 4×4 PNGs named `files` in a fresh folder, as their full paths; green 0.2 shows which way a normal map was read
fn bake_files(test: &str, files: &[&str]) -> Vec<String> {
    let dir = std::env::temp_dir().join(format!("texturecraft-np-baked-maps-{test}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    files
        .iter()
        .map(|file| {
            let path: PathBuf = dir.join(file);
            image::RgbaImage::from_pixel(4, 4, image::Rgba([128, 51, 255, 255])).save(&path).unwrap();
            path.display().to_string()
        })
        .collect()
}

#[test]
fn added_files_are_listed_once_and_clear_empties_the_list() {
    let mut h = harness();
    let paths = bake_files("list", &["Ball_normal.png", "Base_ao.png", "Ball_notes.png"]);

    add_baked_map_files(&mut h.state_mut().ui.new_project, paths.clone());
    add_baked_map_files(&mut h.state_mut().ui.new_project, paths);
    h.run_steps(3);

    assert_eq!(h.state().ui.new_project.baked_maps.len(), 3, "each file once");
    for file in ["Ball_normal.png", "Base_ao.png", "Ball_notes.png"] {
        h.get_by_label(file);
    }

    h.get_by_label("Clear").click();
    h.run_steps(3);
    assert!(h.state().ui.new_project.baked_maps.is_empty());
    assert!(h.query_by_label("Ball_normal.png").is_none());
}

#[test]
fn create_imports_the_listed_maps_in_the_chosen_normal_format() {
    let mut h = harness();
    let paths = bake_files("create", &["Ball_normal.png", "Base_ao.png", "Ball_notes.png"]);
    add_baked_map_files(&mut h.state_mut().ui.new_project, paths);
    h.state_mut().ui.new_project.normal_format = Some(NormalFormat::DirectX);
    h.run_steps(3);

    h.get_by_label("Create").click();
    h.run_steps(3);

    let state = h.state();
    assert!(!state.ui.windows.new_project, "the dialog closes");
    let project = &state.session.doc.project;
    assert_eq!(project.normal_format, NormalFormat::DirectX);
    let baked = |ts: usize, map: BakedMap| state.session.doc.baked.get(&ts).is_some_and(|baked| baked.maps.contains_key(&map));
    assert!(baked(0, BakedMap::Normal) && baked(1, BakedMap::AmbientOcclusion));
    let normal_green = state.session.doc.baked[&0].maps[&BakedMap::Normal][1];
    assert!(normal_green > 0.75, "the normal map is read as DirectX (green flipped): {normal_green}");
    assert_eq!(state.session.doc.resources.len(), 2, "the file naming no map is not imported");
    let toast = &state.toasts.last().unwrap().text;
    assert!(toast.contains("Baked maps imported: 2") && toast.contains("Ball_notes.png"), "{toast}");
    assert!(state.ui.new_project.baked_maps.is_empty(), "the next new project starts with no files");
    assert_eq!(state.session.undo_label(), Some("Import baked maps"));
}
