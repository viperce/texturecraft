//! STL (binary and ascii). Facets keep their own corners, so shading is flat; there are no UVs.

use texturecraft_geom::glam::Vec3;

use super::{RawMesh, check_triangles};
use crate::{Result, malformed};

const KIND: &str = "STL";

pub(crate) fn read(bytes: &[u8]) -> Result<RawMesh> {
    let count = bytes.get(80..84).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let binary_len = count.and_then(|c| c.checked_mul(50)).and_then(|n| n.checked_add(84));
    let looks_ascii = bytes.iter().position(|b| !b.is_ascii_whitespace()).and_then(|i| bytes.get(i..i + 5)) == Some(b"solid".as_slice());
    let tris = match (count, binary_len) {
        // Exact size wins: many binary files also start with "solid".
        (Some(c), Some(len)) if len == bytes.len() => binary(bytes, c)?,
        (Some(c), Some(len)) if len < bytes.len() => binary(bytes, c)?,
        _ if looks_ascii => ascii(bytes)?,
        _ => return Err(malformed(KIND, "neither a complete binary STL nor an ascii STL")),
    };
    let mut raw = RawMesh::default();
    check_triangles(0, tris.len())?;
    let mut normals = Vec::with_capacity(tris.len() * 3);
    for t in &tris {
        let n = (t[1] - t[0]).cross(t[2] - t[0]).try_normalize().unwrap_or(Vec3::Y);
        raw.positions.extend_from_slice(t);
        normals.extend_from_slice(&[n; 3]);
    }
    raw.normals = Some(normals);
    raw.groups.push(("Default".into(), String::new(), (0..raw.positions.len() as u32).collect()));
    Ok(raw)
}

fn binary(bytes: &[u8], count: usize) -> Result<Vec<[Vec3; 3]>> {
    check_triangles(0, count)?;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let f = |k: usize| -> Result<f32> {
            let o = 84 + i * 50 + 12 + k * 4;
            let b = bytes.get(o..o + 4).ok_or_else(|| malformed(KIND, "truncated facet"))?;
            Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        let v = |c: usize| -> Result<Vec3> { Ok(Vec3::new(f(c * 3)?, f(c * 3 + 1)?, f(c * 3 + 2)?)) };
        out.push([v(0)?, v(1)?, v(2)?]);
    }
    Ok(out)
}

fn ascii(bytes: &[u8]) -> Result<Vec<[Vec3; 3]>> {
    let text = std::str::from_utf8(bytes).map_err(|_| malformed(KIND, "ascii STL is not text"))?;
    let mut out = Vec::new();
    let mut facet: Vec<Vec3> = Vec::new();
    let mut words = text.split_whitespace();
    while let Some(w) = words.next() {
        match w {
            "vertex" => {
                let mut c = [0.0f32; 3];
                for v in &mut c {
                    let tok = words.next().ok_or_else(|| malformed(KIND, "truncated vertex"))?;
                    *v = tok.parse().map_err(|_| malformed(KIND, format!("bad number \"{tok}\"")))?;
                }
                facet.push(Vec3::from(c));
            }
            "endfacet" => {
                for i in 1..facet.len().saturating_sub(1) {
                    check_triangles(out.len(), 1)?;
                    out.push([facet[0], facet[i], facet[i + 1]]);
                }
                facet.clear();
            }
            _ => {}
        }
    }
    if out.is_empty() {
        return Err(malformed(KIND, "no facets"));
    }
    Ok(out)
}
