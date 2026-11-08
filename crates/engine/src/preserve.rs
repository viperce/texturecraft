//! Preserve strokes on mesh reimport ("Preserve strokes positions on mesh"): when the
//! reimported mesh's UVs differ, painted pixels follow the surface instead of the UV layout.
//! Each covered texel of the new layout finds the nearest point of the old surface (same
//! material, facing the same way when there is one) and reads the old texel there. Fills and
//! procedurals evaluate on the new mesh by themselves.

use std::collections::HashMap;

use rayon::prelude::*;
use texturecraft_geom::surface_at;
use texturecraft_texel::{TILE, TILE_AREA};

use crate::Session;

/// How far (relative to the larger mesh's bounding diameter) a texel looks for the old surface.
const REACH: f32 = 0.05;

/// Re-project the painted pixels of `next` (a session on the reimported mesh holding `old`'s
/// document) from `old`'s UV layout. Returns how many texels found the old surface;
/// a texture set where none does keeps its pixels as they are. Nothing is re-projected when the
/// UV layout and topology are unchanged (same triangles, materials and UVs): the pixels already
/// sit where they belong, however far the mesh moved.
pub(crate) fn preserve_strokes(old: &Session, next: &mut Session) -> usize {
    if same_layout(&old.mesh, &next.mesh) {
        return 0;
    }
    let reach = REACH * 2.0 * old.mesh.radius().max(next.mesh.radius());
    let mut found = 0;
    for ts in 0..next.set_count() {
        let (Some(oc), Some(nc), Some(set)) = (old.caches.get(ts), next.caches.get(ts), next.doc.project.texture_sets.get(ts)) else { continue };
        if oc.size != nc.size {
            continue;
        }
        let ((w, h), material) = (nc.size, set.material);
        let (omesh, nmesh, bvh, gbuf) = (&old.mesh, &next.mesh, &old.bvh, &nc.gbuf);
        let mine = |t: u32| omesh.material_of(t as usize) == Some(material);
        // Old texel position (NaN: none) per texel of each covered destination tile.
        let table: HashMap<(u32, u32), Vec<[f32; 2]>> = nc
            .covered_tiles
            .par_iter()
            .map(|&(tx, ty)| {
                let mut src = vec![[f32::NAN; 2]; TILE_AREA];
                for (k, slot) in src.iter_mut().enumerate() {
                    let (x, y) = (tx * TILE + k as u32 % TILE, ty * TILE + k as u32 / TILE);
                    let Some(s) = (x < w && y < h).then(|| gbuf.surface(nmesh, (y * w + x) as usize)).flatten() else { continue };
                    let facing = |t: u32| {
                        mine(t)
                            && omesh.tri(t as usize).is_some_and(|[a, b, c]| {
                                let p = |i: usize| omesh.positions.get(i).copied().unwrap_or_default();
                                (p(b) - p(a)).cross(p(c) - p(a)).dot(s.geo_nrm) > 0.0
                            })
                    };
                    let hit = bvh.nearest(s.pos, reach, &facing).or_else(|| bvh.nearest(s.pos, reach, &mine));
                    if let Some(o) = hit.and_then(|hit| surface_at(omesh, hit.tri, hit.u, hit.v)) {
                        *slot = [o.uv.x * w as f32, o.uv.y * h as f32];
                    }
                }
                ((tx, ty), src)
            })
            .collect();
        let matched = table.values().flatten().filter(|p| p[0].is_finite()).count();
        found += matched;
        // No old surface within reach anywhere (a moved or unrelated mesh): the paint stays where it is.
        if matched == 0 {
            continue;
        }
        let map = |x: f32, y: f32| {
            let (xi, yi) = (x as u32, y as u32);
            table.get(&(xi / TILE, yi / TILE)).and_then(|t| t.get(((yi % TILE) * TILE + xi % TILE) as usize)).filter(|p| p[0].is_finite()).map(|p| (p[0], p[1]))
        };
        let tiles = nc.covered_tiles.clone();
        crate::layer_ops::map_set_images(&mut next.doc, ts, |img, empty| crate::uv_tile_cmds::remap_image(img, (w, h), empty, &map, &tiles, false));
    }
    found
}

/// Same index buffer, same UVs per vertex and the same material ranges.
fn same_layout(a: &texturecraft_geom::Mesh, b: &texturecraft_geom::Mesh) -> bool {
    let ranges = |m: &texturecraft_geom::Mesh| m.submeshes.iter().map(|s| (s.material, s.start, s.count)).collect::<Vec<_>>();
    a.indices == b.indices && a.uvs == b.uvs && ranges(a) == ranges(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Document;
    use texturecraft_core::Channel;
    use texturecraft_geom::glam::Vec2;
    use texturecraft_geom::primitives;

    #[test]
    fn strokes_follow_the_surface_when_uvs_change() {
        let mut s = Session::new_project("p", primitives::builtin("plane").unwrap(), "builtin:plane", None, 128).unwrap();
        let id = s.doc.project.texture_sets[0].stack[0];
        // Paint the texels whose surface point has x > 0 (one half of the plane).
        let gbuf = s.caches[0].gbuf.clone();
        let img = s.doc.pixels.layer_mut(id, Channel::BaseColor, (128, 128));
        for i in 0..128 * 128 {
            if gbuf.surface(&s.mesh, i).is_some_and(|p| p.pos.x > 0.0) {
                img.set(i as u32 % 128, i as u32 / 128, &[65535, 0, 0, 65535]);
            }
        }
        // The same plane with its UVs mirrored in u.
        let mut mesh = (*s.mesh).clone();
        for uv in &mut mesh.uvs {
            *uv = Vec2::new(1.0 - uv.x, uv.y);
        }
        let doc: Document = s.doc.clone();
        let mut next = Session::new(doc, mesh, None).unwrap();
        assert!(preserve_strokes(&s, &mut next) > 128 * 100);
        let img = next.doc.pixels.layer(id, Channel::BaseColor).unwrap();
        let gbuf = &next.caches[0].gbuf;
        let (mut right, mut wrong) = (0, 0);
        for i in 0..128 * 128 {
            let Some(p) = gbuf.surface(&next.mesh, i) else { continue };
            if p.pos.x.abs() < 0.05 {
                continue;
            }
            let painted = img.get(i as u32 % 128, i as u32 / 128)[3] > 32768;
            if painted == (p.pos.x > 0.0) { right += 1 } else { wrong += 1 }
        }
        assert!(right > 10_000 && wrong == 0, "{right} right, {wrong} wrong");
    }

    #[test]
    fn a_mesh_with_no_surface_in_reach_keeps_the_paint_where_it_is() {
        let mut s = Session::new_project("p", primitives::builtin("plane").unwrap(), "builtin:plane", None, 128).unwrap();
        let id = s.doc.project.texture_sets[0].stack[0];
        let img = s.doc.pixels.layer_mut(id, Channel::BaseColor, (128, 128));
        for x in 0..64 {
            img.set(x, 40, &[65535, 0, 0, 65535]);
        }
        // The same plane far away and with mirrored UVs (so the layout differs and the unchanged-layout
        // shortcut is not taken): nothing of the old surface is within reach.
        let mut mesh = (*s.mesh).clone();
        for p in &mut mesh.positions {
            p.x += 100.0;
        }
        for uv in &mut mesh.uvs {
            *uv = Vec2::new(1.0 - uv.x, uv.y);
        }
        let mut next = Session::new(s.doc.clone(), mesh, None).unwrap();
        assert_eq!(preserve_strokes(&s, &mut next), 0);
        let img = next.doc.pixels.layer(id, Channel::BaseColor).unwrap();
        assert_eq!(img.get(10, 40)[3], 65535, "the painted texel is still there");
    }

    #[test]
    fn a_translated_mesh_with_the_same_uvs_keeps_every_pixel() {
        let mut s = Session::new_project("p", primitives::builtin("plane").unwrap(), "builtin:plane", None, 128).unwrap();
        let id = s.doc.project.texture_sets[0].stack[0];
        let img = s.doc.pixels.layer_mut(id, Channel::BaseColor, (128, 128));
        for x in 0..128 {
            img.set(x, 40, &[65535, 0, 0, 65535]);
        }
        // Slid along its own surface by a fraction of the plane: half of it still overlaps the old one.
        let mut mesh = (*s.mesh).clone();
        let shift = mesh.radius() * 0.5;
        for p in &mut mesh.positions {
            p.x += shift;
        }
        let mut next = Session::new(s.doc.clone(), mesh, None).unwrap();
        assert_eq!(preserve_strokes(&s, &mut next), 0);
        let img = next.doc.pixels.layer(id, Channel::BaseColor).unwrap();
        for x in 0..128 {
            assert_eq!(img.get(x, 40)[3], 65535, "texel {x} of the painted row moved");
            assert_eq!(img.get(x, 41)[3], 0, "texel {x} of the next row got paint");
        }
    }
}
