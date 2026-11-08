//! Importing baked map files at once: each lands in the texture set and mesh map its name names

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use texturecraft_engine::core::BakedMap;
use texturecraft_engine::geom::primitives;
use texturecraft_engine::{Session, execute};

/// The material ball: texture sets "Ball" and "Base"
fn session() -> Session {
    Session::new_project("t", primitives::material_ball(), "builtin:material_ball", None, 128).unwrap()
}

/// A folder of 4×4 gray PNGs named `files`, each with its own gray level
fn bake_folder(test: &str, files: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("texturecraft-meshmap-import-{test}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (index, file) in files.iter().enumerate() {
        let gray = u8::try_from(20 + 20 * index).unwrap_or(u8::MAX);
        image::RgbaImage::from_pixel(4, 4, image::Rgba([gray, gray, gray, 255])).save(dir.join(file)).unwrap();
    }
    dir
}

fn import(s: &mut Session, dir: &Path, files: &[(&str, Option<&str>)]) -> Value {
    let files: Vec<Value> = files
        .iter()
        .map(|(file, map)| {
            let path = dir.join(file).display().to_string();
            match map {
                Some(map) => json!({"path": path, "map": map}),
                None => json!({"path": path}),
            }
        })
        .collect();
    execute(s, "textureSet.meshMap.import", &json!({ "files": files })).unwrap()
}

fn file_names(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap_or_else(|| entry["path"].as_str().unwrap()))
        .map(|path| path.rsplit(['/', '\\']).next().unwrap().to_string())
        .collect()
}

fn has_map(s: &Session, ts: usize, map: BakedMap) -> bool {
    s.doc.baked.get(&ts).is_some_and(|baked| baked.maps.contains_key(&map)) && s.doc.project.texture_sets[ts].baked.contains(&map)
}

#[test]
fn files_go_to_the_set_and_map_their_names_name() {
    let files = ["Ball_normal.png", "Ball_world_normal.png", "base-AO.png", "Base_curvature.png", "Ball_diffuse.png", "Head_ao.png", "Base_id.png"];
    let dir = bake_folder("sets", &files);
    let mut s = session();
    let label_before = s.undo_label().map(str::to_string);

    let result = import(&mut s, &dir, &files.map(|file| (file, None)));

    let assigned = &result["assigned"];
    assert_eq!(file_names(assigned).len(), 5, "{result}");
    for (ts, map) in [(0, BakedMap::Normal), (0, BakedMap::WorldNormal), (1, BakedMap::AmbientOcclusion), (1, BakedMap::Curvature), (1, BakedMap::Id)] {
        assert!(has_map(&s, ts, map), "set {ts} lacks {}", map.id());
        assert!(assigned.as_array().unwrap().iter().any(|entry| entry["ts"] == ts && entry["map"] == map.id()), "{result}");
    }
    assert_eq!(file_names(&result["unmatched"]), ["Ball_diffuse.png", "Head_ao.png"]);
    assert_eq!(s.undo_label(), Some("Import baked maps"));

    execute(&mut s, "edit.undo", &Value::Null).unwrap();
    assert_eq!(s.undo_label().map(str::to_string), label_before, "one undo step");
    assert!(s.doc.baked.values().all(|baked| baked.maps.is_empty()), "undo removes every imported map");
    assert!(s.doc.resources.is_empty(), "undo removes the imported image resources");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_given_map_overrides_the_name() {
    let dir = bake_folder("override", &["Ball_bake3.png", "Base_normal.png"]);
    let mut s = session();

    let result = import(&mut s, &dir, &[("Ball_bake3.png", Some("thickness")), ("Base_normal.png", Some("height"))]);

    assert_eq!(file_names(&result["assigned"]).len(), 2, "{result}");
    assert!(has_map(&s, 0, BakedMap::Thickness));
    assert!(has_map(&s, 1, BakedMap::Height));
    assert!(!has_map(&s, 1, BakedMap::Normal));
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_second_file_for_one_slot_is_left_out() {
    let dir = bake_folder("twice", &["Ball_ao.png", "Ball_occlusion.png"]);
    let mut s = session();

    let result = import(&mut s, &dir, &[("Ball_ao.png", None), ("Ball_occlusion.png", None)]);

    assert_eq!(file_names(&result["assigned"]), ["Ball_ao.png"]);
    assert_eq!(file_names(&result["unmatched"]), ["Ball_occlusion.png"]);
    assert_eq!(s.doc.resources.len(), 1, "the left-out file is not imported");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn an_undecodable_file_imports_nothing() {
    let dir = bake_folder("undecodable", &["Ball_ao.png"]);
    std::fs::write(dir.join("Base_ao.png"), b"not an image").unwrap();
    let mut s = session();
    let label_before = s.undo_label().map(str::to_string);
    let files = json!({"files": [{"path": dir.join("Ball_ao.png").display().to_string()}, {"path": dir.join("Base_ao.png").display().to_string()}]});

    let error = execute(&mut s, "textureSet.meshMap.import", &files).unwrap_err().to_string();

    assert!(error.contains("Base_ao.png"), "{error}");
    assert!(s.doc.baked.values().all(|baked| baked.maps.is_empty()));
    assert!(s.doc.resources.is_empty() && s.images.is_empty());
    assert_eq!(s.undo_label().map(str::to_string), label_before);
    std::fs::remove_dir_all(dir).ok();
}

/// The UV tile plane: one texture set "Plane" on tiles 1001 and 1002
fn tiled_session() -> Session {
    Session::new_project("t", primitives::udim_plane(), "builtin:udim_plane", None, 128).unwrap()
}

#[test]
fn a_uv_tiled_set_takes_one_file_per_tile_however_each_is_written() {
    let files = ["Plane_ao.1001.png", "plane-AO-1002.png"];
    let dir = bake_folder("tiles", &files);
    let mut s = tiled_session();

    let result = import(&mut s, &dir, &files.map(|file| (file, None)));

    assert_eq!(file_names(&result["assigned"]).len(), 2, "{result}");
    let baked = &s.doc.baked[&0];
    let ao = &baked.maps[&BakedMap::AmbientOcclusion];
    let width = baked.resolution as usize;
    let middle_row = ao.len() / width / 2 * width;
    assert!(ao[middle_row + width / 4] < ao[middle_row + 3 * width / 4], "each tile reads its own file");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn tiled_files_without_their_tiles_are_left_out_alone() {
    let files = ["Plane_ao.1001.png", "Plane_ao.1002.png", "Plane_normal.png", "Plane_curvature.1001.png", "Plane_id.1005.png"];
    let dir = bake_folder("partial-tiles", &files);
    let mut s = tiled_session();

    let result = import(&mut s, &dir, &files.map(|file| (file, None)));

    assert_eq!(file_names(&result["assigned"]), ["Plane_ao.1001.png", "Plane_ao.1002.png"]);
    assert_eq!(file_names(&result["unmatched"]), ["Plane_normal.png", "Plane_id.1005.png", "Plane_curvature.1001.png"]);
    assert!(has_map(&s, 0, BakedMap::AmbientOcclusion));
    assert_eq!(s.doc.resources.len(), 2, "left-out files are not imported");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn nothing_matched_makes_no_undo_step() {
    let dir = bake_folder("none", &["Head_ao.png", "Ball_diffuse.png"]);
    let mut s = session();
    let label_before = s.undo_label().map(str::to_string);

    let result = import(&mut s, &dir, &[("Head_ao.png", None), ("Ball_diffuse.png", None)]);

    assert_eq!(file_names(&result["assigned"]), Vec::<String>::new());
    assert_eq!(file_names(&result["unmatched"]).len(), 2);
    assert_eq!(s.undo_label().map(str::to_string), label_before);
    assert!(!s.modified);
    std::fs::remove_dir_all(dir).ok();
}
