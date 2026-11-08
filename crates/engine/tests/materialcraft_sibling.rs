//! A launcher install puts each app in its own folder: `<root>/TextureCraft` and `<root>/MaterialCraft`.

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn finds_materialcraft_in_sibling_app_folder() {
    let exe = std::env::current_exe().unwrap();
    let root = exe.parent().unwrap().parent().unwrap();
    let dir = root.join("MaterialCraft");
    let name = "mc-sibling-probe";
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(&file), b"").unwrap();
    let found = texturecraft_engine::link_cmds::materialcraft_tool(name, "TEXTURECRAFT_TEST_UNSET_ENV");
    std::fs::remove_file(dir.join(&file)).unwrap();
    let _ = std::fs::remove_dir(&dir);
    assert_eq!(found, dir.join(&file));
}
