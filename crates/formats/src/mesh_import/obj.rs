//! Wavefront OBJ (via `tobj`): polygons fan-triangulated, one vertex per unique v/vt/vn.

use texturecraft_geom::glam::{Vec2, Vec3, Vec4};

use super::{RawMesh, check_triangles, check_vertices};
use crate::{Result, malformed};

const KIND: &str = "OBJ";

pub(crate) fn read(bytes: &[u8]) -> Result<RawMesh> {
    crate::guard(KIND, || read_inner(bytes))
}

fn read_inner(bytes: &[u8]) -> Result<RawMesh> {
    let text = String::from_utf8_lossy(bytes);
    let filled = fill_missing_vt(&text);
    let text = filled.as_deref().unwrap_or(&text);
    // Material names come from `usemtl` (MTL files are never read): tobj only assigns a face's
    // material when a library defines it, so every library request gets a synthetic one.
    let mut names: Vec<&str> = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.split_whitespace().next() == Some("usemtl")
            && let Some((_, name)) = line.split_once(' ')
            && !name.trim().is_empty()
            && !names.contains(&name.trim())
        {
            names.push(name.trim());
        }
    }
    let library: String = names.iter().map(|n| format!("newmtl {n}\n")).collect();
    let source = format!("mtllib texturecraft-usemtl.mtl\n{text}");
    // Polygons stay whole so the wireframe can show them; `RawMesh::polygon` triangulates.
    let opts = tobj::LoadOptions { single_index: true, triangulate: false, ignore_points: true, ignore_lines: true };
    let (models, materials) =
        tobj::load_obj_buf(&mut source.as_bytes(), &opts, |_| tobj::load_mtl_buf(&mut library.as_bytes())).map_err(|e| malformed(KIND, e))?;
    let materials = materials.unwrap_or_default();

    let mut raw = RawMesh::default();
    if filled.is_some() {
        raw.warnings.push("some faces have no UVs; their corners were placed at UV (0, 0)".into());
    }
    let (mut normals, mut uvs, mut colors) = (Some(Vec::new()), Some(Vec::new()), Vec::new());
    let mut partial_uvs = false;
    for model in &models {
        let m = &model.mesh;
        let n = m.positions.len() / 3;
        check_vertices(raw.positions.len().saturating_add(n))?;
        // Corners per face; without arities every face is a triangle.
        let arities: Vec<usize> = if m.face_arities.is_empty() { vec![3; m.indices.len() / 3] } else { m.face_arities.iter().map(|a| *a as usize).collect() };
        check_triangles(raw.triangle_count(), arities.iter().map(|a| a.saturating_sub(2)).sum())?;
        let base = raw.positions.len() as u32;
        raw.positions.extend(m.positions.chunks_exact(3).map(|p| Vec3::new(p[0], p[1], p[2])));
        if m.normals.len() == n * 3 {
            if let Some(v) = &mut normals {
                v.extend(m.normals.chunks_exact(3).map(|p| Vec3::new(p[0], p[1], p[2])));
            }
        } else {
            normals = None;
        }
        if m.texcoords.len() == n * 2 {
            if let Some(v) = &mut uvs {
                // OBJ UVs have their origin bottom-left.
                v.extend(m.texcoords.chunks_exact(2).map(|t| Vec2::new(t[0], 1.0 - t[1])));
            }
        } else {
            partial_uvs |= n > 0 && !m.indices.is_empty();
            uvs = None;
        }
        // `v x y z r g b`: objects without colors are white.
        match m.vertex_color.len() == n * 3 {
            true => colors.extend(m.vertex_color.chunks_exact(3).map(|c| Vec4::new(c[0], c[1], c[2], 1.0))),
            false => colors.extend(std::iter::repeat_n(Vec4::ONE, n)),
        }
        let name = match m.material_id {
            Some(id) => materials.get(id).map(|mat| mat.name.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| format!("Material_{id}")),
            None => "Default".to_string(),
        };
        if let Some(i) = m.indices.iter().find(|i| **i as usize >= n) {
            return Err(malformed(KIND, format!("face index {i} out of range")));
        }
        // tobj names models without `o`/`g` "unnamed_object"; an empty name takes the file stem.
        let part = if model.name == "unnamed_object" { "" } else { model.name.as_str() };
        let (mut rest, mut corners) = (m.indices.as_slice(), Vec::new());
        for arity in arities {
            let Some((face, tail)) = rest.split_at_checked(arity) else { break };
            rest = tail;
            corners.clear();
            corners.extend(face.iter().map(|i| base + i));
            if arity >= 3 {
                raw.polygon(&name, part, &corners);
            }
        }
    }
    if partial_uvs && models.iter().any(|m| !m.mesh.texcoords.is_empty()) {
        raw.warnings.push("some objects have no UVs; the whole mesh was unwrapped automatically".into());
    }
    raw.normals = normals;
    raw.uvs = uvs;
    raw.colors = models.iter().any(|m| !m.mesh.vertex_color.is_empty()).then_some(colors);
    Ok(raw)
}

/// tobj drops the UV of a corner without `vt`, which misaligns the object's UVs. When a file has
/// UVs but some face corners lack one, a `vt 0 0` is put first (positive vt indices shift by one;
/// negative ones stay relative) and those corners point at it. `None` when nothing is missing.
fn fill_missing_vt(text: &str) -> Option<String> {
    let face = |line: &str| line.split_whitespace().next() == Some("f");
    // An inline `# comment` ends the corner list; its words are not corners.
    fn corners(line: &str) -> impl Iterator<Item = &str> {
        line.split_once('#').map_or(line, |(body, _)| body).split_whitespace().skip(1)
    }
    let has_vt = text.lines().any(|l| l.split_whitespace().next() == Some("vt"));
    let missing = || text.lines().filter(|l| face(l)).flat_map(corners).any(|t| t.split('/').nth(1).is_none_or(str::is_empty));
    if !has_vt || !missing() {
        return None;
    }
    let mut out = String::with_capacity(text.len() + text.len() / 8 + 8);
    out.push_str("vt 0 0\n");
    for line in text.lines() {
        if !face(line) {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        out.push('f');
        for token in corners(line) {
            let mut parts = token.splitn(3, '/');
            let (v, vt, vn) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default(), parts.next());
            let vt = match vt.parse::<i64>() {
                Ok(i) if i > 0 => i.saturating_add(1).to_string(),
                _ if vt.is_empty() => "1".to_string(),
                _ => vt.to_string(),
            };
            out.push(' ');
            out.push_str(v);
            out.push('/');
            out.push_str(&vt);
            if let Some(vn) = vn {
                out.push('/');
                out.push_str(vn);
            }
        }
        // tobj rejects words after the corners, so the comment moves to its own line.
        if let Some((_, comment)) = line.split_once('#') {
            out.push_str("\n#");
            out.push_str(comment);
        }
        out.push('\n');
    }
    Some(out)
}
