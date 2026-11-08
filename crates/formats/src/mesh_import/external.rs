//! Meshes that keep part of their data in other files: a `.gltf`'s buffers and images, a loose
//! USD layer's textures, an FBX's texture files. [`self_contained_mesh`] folds those files in, so a project stores the
//! mesh as one file and imports it again from its bytes alone.

use base64::Engine as _;
use serde_json::Value;

use super::MAX_MESH_FILE_BYTES;
use crate::{FormatError, Result, malformed};

/// A mesh file ready to import from its bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshFile {
    /// The name to import it by: the original, or `.usdz` for a loose USD layer packaged with its
    /// textures.
    pub name: String,
    pub bytes: Vec<u8>,
    /// Referenced files that could not be read, as messages for the log.
    pub warnings: Vec<String>,
}

/// `bytes` of mesh `name` with the files it references folded in: a `.gltf`'s external buffers
/// and images become data URIs, a `.usd`/`.usda`/`.usdc` layer whose materials use texture files
/// becomes a `.usdz` package holding them, and an FBX whose base color textures are files becomes
/// a package of it and them (still named `.fbx`). `read` gets a referenced file by its path below
/// the mesh's folder (`/`-separated); files outside that folder (absolute paths, URLs, `..` above
/// it) are not read. Other formats, and meshes that reference nothing, come back unchanged.
pub fn self_contained_mesh(name: &str, bytes: Vec<u8>, read: &mut dyn FnMut(&str) -> Option<Vec<u8>>) -> Result<MeshFile> {
    let mut warnings = Vec::new();
    let mut found = |asset: &str| {
        let data = below(asset).and_then(|p| read(&p));
        let msg = format!("{name}: {asset} was not found next to the mesh");
        if data.is_none() && !warnings.contains(&msg) {
            warnings.push(msg);
        }
        data
    };
    let (name, bytes) = match super::extension(name).as_str() {
        "gltf" => (name.to_string(), inline_gltf(bytes, &mut found)?),
        "fbx" => {
            let (package, missing) = super::fbx::package(name, &bytes, &mut |asset| below(asset).and_then(|p| read(&p)))?;
            warnings.extend(missing.into_iter().map(|asset| format!("{name}: {asset} was not found next to the mesh")));
            (name.to_string(), package.unwrap_or(bytes))
        }
        "usd" | "usda" | "usdc" => match super::usd::package(name, &bytes, &mut found)? {
            Some(usdz) => (std::path::Path::new(name).with_extension("usdz").to_string_lossy().into_owned(), usdz),
            None => (name.to_string(), bytes),
        },
        _ => (name.to_string(), bytes),
    };
    Ok(MeshFile { name, bytes, warnings })
}

/// Read mesh file `path` with the files it references from its folder (see
/// [`self_contained_mesh`]), each at most [`MAX_MESH_FILE_BYTES`].
#[cfg(not(target_arch = "wasm32"))]
pub fn read_mesh_file(path: &std::path::Path) -> Result<MeshFile> {
    let bytes = crate::read_capped(path, MAX_MESH_FILE_BYTES)?;
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new(""));
    self_contained_mesh(&path.to_string_lossy(), bytes, &mut |rel| read_inside(dir, rel))
}

/// `rel` (already checked by [`below`]) read from below `dir`, or None when it is missing, too big,
/// or, once symbolic links and junctions are followed, is not inside `dir`: `below` only looks at
/// the text of the path, so a link in the folder could otherwise lead to any file.
#[cfg(not(target_arch = "wasm32"))]
fn read_inside(dir: &std::path::Path, rel: &str) -> Option<Vec<u8>> {
    let root = std::fs::canonicalize(if dir.as_os_str().is_empty() { std::path::Path::new(".") } else { dir }).ok()?;
    let file = std::fs::canonicalize(dir.join(rel)).ok()?;
    if !file.starts_with(&root) {
        return None;
    }
    crate::read_capped(&file, MAX_MESH_FILE_BYTES).ok()
}

/// `asset` as a path below the mesh's folder (`/`-separated, `.` and `..` resolved), or None when
/// it is absolute, has a URL scheme or drive letter, or leaves the folder.
pub(crate) fn below(asset: &str) -> Option<String> {
    let s = asset.replace('\\', "/");
    if s.starts_with('/') || s.contains(':') {
        return None;
    }
    let mut parts = Vec::new();
    for c in s.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// A `.gltf` with its external buffers and images as base64 data URIs (unchanged when it has none).
fn inline_gltf(bytes: Vec<u8>, found: &mut dyn FnMut(&str) -> Option<Vec<u8>>) -> Result<Vec<u8>> {
    let mut doc: Value = serde_json::from_slice(&bytes).map_err(|e| malformed("glTF", e))?;
    let mut total = 0u64;
    let mut changed = false;
    for key in ["buffers", "images"] {
        let Some(items) = doc.get_mut(key).and_then(Value::as_array_mut) else { continue };
        for item in items {
            let Some(uri) = item.get("uri").and_then(Value::as_str) else { continue };
            if uri.starts_with("data:") {
                continue;
            }
            let path = super::gltf::percent_decode(uri).ok_or_else(|| malformed("glTF", format!("URI \"{uri}\" is not valid UTF-8")))?;
            let Some(data) = found(&path) else { continue };
            // Base64 grows the data by a third; the result must stay importable.
            total = total.saturating_add(data.len() as u64);
            if total.saturating_mul(4) / 3 > MAX_MESH_FILE_BYTES {
                return Err(FormatError::TooLarge("the glTF and the files it references exceed 2 GiB".into()));
            }
            let mime = match item.get("mimeType").and_then(Value::as_str) {
                Some(m) => m.to_string(),
                None if key == "buffers" => "application/octet-stream".into(),
                None => image_mime(&path).into(),
            };
            item["uri"] = Value::String(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(&data)));
            changed = true;
        }
    }
    if !changed {
        return Ok(bytes);
    }
    serde_json::to_vec(&doc).map_err(|e| FormatError::Invalid(e.to_string()))
}

fn image_mime(path: &str) -> &'static str {
    match super::extension(path).as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        _ => "image/png",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link `link` inside `dir` leading to `target` (a folder): a symbolic link on Unix, a junction
    /// on Windows. False when the system refuses to make one.
    #[cfg(not(target_arch = "wasm32"))]
    fn link_dir(link: &std::path::Path, target: &std::path::Path) -> bool {
        #[cfg(unix)]
        return std::os::unix::fs::symlink(target, link).is_ok();
        #[cfg(windows)]
        return std::process::Command::new("cmd.exe").args(["/D", "/C", "mklink", "/J"]).arg(link).arg(target).output().is_ok_and(|o| o.status.success());
        #[cfg(not(any(unix, windows)))]
        return false;
    }

    /// Files are read through ordinary paths and links that stay inside the folder, but not through a link
    /// (folder or file) that leads outside it.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_link_in_the_mesh_folder_cannot_lead_outside_it() {
        let base = std::env::temp_dir().join(format!("tc-mesh-link-{}", std::process::id()));
        let (dir, outside) = (base.join("model"), base.join("secret"));
        std::fs::create_dir_all(dir.join("tex")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("key.txt"), b"private").unwrap();
        std::fs::write(dir.join("tex/a.png"), b"inside").unwrap();
        assert_eq!(read_inside(&dir, "tex/a.png").as_deref(), Some(&b"inside"[..]), "ordinary files below the folder are read");
        assert_eq!(read_inside(&dir, "tex/missing.png"), None);
        if link_dir(&dir.join("out"), &outside) {
            assert_eq!(read_inside(&dir, "out/key.txt"), None, "through a link to another folder");
        } else {
            eprintln!("skipped the link case: this system would not make a link");
        }
        // A link that stays inside the folder is an ordinary shortcut and still works.
        if link_dir(&dir.join("shortcut"), &dir.join("tex")) {
            assert_eq!(read_inside(&dir, "shortcut/a.png").as_deref(), Some(&b"inside"[..]), "through a link to a folder inside");
        }
        // A link to a single outside file (Unix symbolic links; Windows file links need privileges).
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.join("key.txt"), dir.join("tex/key.png")).unwrap();
            assert_eq!(read_inside(&dir, "tex/key.png"), None, "through a link to an outside file");
        }
        // The mesh's own folder reached through a link is fine: both sides are resolved.
        if link_dir(&base.join("alias"), &dir) {
            assert_eq!(read_inside(&base.join("alias"), "tex/a.png").as_deref(), Some(&b"inside"[..]), "folder given through a link");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn below_keeps_paths_inside_the_folder() {
        assert_eq!(below("tex/a.png").as_deref(), Some("tex/a.png"));
        assert_eq!(below("./tex\\b.png").as_deref(), Some("tex/b.png"));
        assert_eq!(below("tex/../c.png").as_deref(), Some("c.png"));
        for outside in ["../a.png", "/etc/passwd", "C:/x.png", "C:x.png", "http://host/a.png", "file:///a.png", "", ".", "tex/../.."] {
            assert_eq!(below(outside), None, "{outside}");
        }
    }

    #[test]
    fn other_formats_and_self_contained_files_are_unchanged() {
        let mut read = |_: &str| -> Option<Vec<u8>> { panic!("nothing to read") };
        let obj = b"v 0 0 0\n".to_vec();
        assert_eq!(self_contained_mesh("a.obj", obj.clone(), &mut read).unwrap(), MeshFile { name: "a.obj".into(), bytes: obj, warnings: vec![] });
        let gltf = br#"{"asset": {"version": "2.0"}, "buffers": [{"uri": "data:application/octet-stream;base64,AAAA", "byteLength": 3}]}"#.to_vec();
        assert_eq!(self_contained_mesh("a.gltf", gltf.clone(), &mut read).unwrap().bytes, gltf);
    }
}
