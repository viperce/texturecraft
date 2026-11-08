//! Stamps → stroke alpha in texture space. Every texel whose surface point lies inside a
//! stamp's oriented volume receives paint, so UV seams are seamless by construction.

use std::collections::BTreeSet;

use rayon::prelude::*;

use texturecraft_geom::glam::{Vec2, Vec3};
use texturecraft_geom::{Bvh, Mesh, NO_TRI, UvGBuffer};
use texturecraft_texel::{TILE, TileImage, from_u16, to_u16};

use crate::brush::Shape;
use crate::pickup::Pickup;
use crate::stroke::Stamp;
use crate::visibility::EyeDepth;

/// The stroke alpha being accumulated for one texture set.
#[derive(Clone, Debug)]
pub struct StrokeCanvas {
    pub alpha: TileImage,
    /// Every tile the stroke touched.
    pub dirty: BTreeSet<(u32, u32)>,
    /// Tiles touched since the caller last cleared this set (incremental preview updates).
    pub recent: BTreeSet<(u32, u32)>,
    /// Quick mask: paint only where this coverage image (1 component) is set; inverted when
    /// the flag is true.
    pub limit: Option<(TileImage, bool)>,
    /// Clone and smudge: the channel values the stroke carries.
    pub pickup: Option<Pickup>,
    /// The stamp being added: `(x, y, alpha)` per texel (reused between stamps).
    texels: Vec<(u32, u32, f32)>,
}

impl StrokeCanvas {
    pub fn new(width: u32, height: u32) -> StrokeCanvas {
        StrokeCanvas { alpha: TileImage::new(width, height, 1), dirty: BTreeSet::new(), recent: BTreeSet::new(), limit: None, pickup: None, texels: Vec::new() }
    }

    /// Start over when the canvas is not the size of `gbuf`.
    fn fit(&mut self, gbuf: &UvGBuffer) {
        if (self.alpha.width(), self.alpha.height()) != (gbuf.width, gbuf.height) {
            *self = StrokeCanvas::new(gbuf.width, gbuf.height);
        }
    }

    fn touch(&mut self, x: u32, y: u32) {
        let t = (x / TILE, y / TILE);
        self.dirty.insert(t);
        self.recent.insert(t);
    }

    /// Blend the gathered stamp texels (stamp center `center` in texels) into the stroke: the
    /// quick mask scales them, then the pickup deposits its values or the alpha accumulates
    /// ("over": `m = a + m·(1 − a)`).
    fn add_stamp(&mut self, center: [f32; 2]) {
        let mut texels = std::mem::take(&mut self.texels);
        if let Some((mask, invert)) = &self.limit {
            for t in &mut texels {
                let m = from_u16(mask.get(t.0, t.1)[0]);
                t.2 *= if *invert { 1.0 - m } else { m };
            }
        }
        let painted = match &mut self.pickup {
            Some(p) => p.stamp(&mut self.alpha, &texels, center),
            None => {
                for &(x, y, a) in &texels {
                    let m = from_u16(self.alpha.get(x, y)[0]);
                    self.alpha.set(x, y, &[to_u16(a + m * (1.0 - a))]);
                }
                true
            }
        };
        if painted {
            for &(x, y, a) in &texels {
                if a > 0.0 {
                    self.touch(x, y);
                }
            }
        }
        texels.clear();
        self.texels = texels;
    }
}

/// What the rasterizer reads.
pub struct RasterCtx<'a> {
    pub mesh: &'a Mesh,
    pub bvh: &'a Bvh,
    pub gbuf: &'a UvGBuffer,
    pub material: usize,
    /// Camera visibility (None paints hidden surfaces too, e.g. for scripted strokes).
    pub depth: Option<&'a EyeDepth>,
    pub eye: Vec3,
    pub backface_culling: bool,
    /// Angle fade from/to in degrees.
    pub angle_fade: (f32, f32),
    pub shape: Shape<'a>,
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0).max(1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Accumulate `stamps` into `canvas` ("over": `m = a + m·(1 − a)`).
pub fn rasterize(canvas: &mut StrokeCanvas, ctx: &RasterCtx, stamps: &[Stamp]) {
    let (w, h) = (ctx.gbuf.width, ctx.gbuf.height);
    canvas.fit(ctx.gbuf);
    let fade_lo = ctx.angle_fade.1.to_radians().cos();
    let fade_hi = ctx.angle_fade.0.to_radians().cos();
    let mut tris = Vec::new();
    for s in stamps {
        if s.radius.is_nan() || s.radius <= 0.0 || !s.center.is_finite() {
            continue;
        }
        let ext = Vec3::splat(s.radius * s.depth.max(1.0) * 1.42);
        tris.clear();
        ctx.bvh.query_box(s.center - ext, s.center + ext, |t| tris.push(t));
        // Triangles scan independently (in parallel for large stamps); stamp alphas combine
        // commutatively, so the order does not matter.
        let scan = |&t: &u32| scan_triangle(ctx, s, t, w, h, fade_lo, fade_hi);
        let parts: Vec<TriTexels> = if tris.len() > 16 { tris.par_iter().filter_map(scan).collect() } else { tris.iter().filter_map(scan).collect() };
        // The stamp center in texels: the footprint texel nearest to it on the surface.
        let mut center = ([0.0f32; 2], f32::MAX);
        let mut shared = false;
        for (texels, c, sh) in parts {
            if c.1 < center.1 {
                center = c;
            }
            shared |= sh;
            canvas.texels.extend(texels);
        }
        if shared {
            // Texels reached through shared UVs can also come from a neighbouring triangle (on
            // their common edge): keep each texel once, with its strongest alpha.
            canvas.texels.sort_unstable_by(|a, b| (a.1, a.0).cmp(&(b.1, b.0)).then(b.2.total_cmp(&a.2)));
            canvas.texels.dedup_by_key(|t| (t.0, t.1));
        }
        canvas.add_stamp(center.0);
    }
}

/// Texels of one triangle a stamp covers, and the one nearest the stamp center.
/// The flag is set when some texels are owned by another triangle in the G-buffer (shared UVs).
type TriTexels = (Vec<(u32, u32, f32)>, ([f32; 2], f32), bool);

#[allow(clippy::too_many_arguments)]
fn scan_triangle(ctx: &RasterCtx, s: &Stamp, t: u32, w: u32, h: u32, fade_lo: f32, fade_hi: f32) -> Option<TriTexels> {
    if ctx.mesh.material_of(t as usize) != Some(ctx.material) {
        return None;
    }
    let [a, b, c] = ctx.mesh.tri(t as usize)?;
    let (ua, ub, uc) = (ctx.mesh.uvs.get(a)?, ctx.mesh.uvs.get(b)?, ctx.mesh.uvs.get(c)?);
    let (pa, pb, pc) = (ctx.mesh.positions.get(a)?, ctx.mesh.positions.get(b)?, ctx.mesh.positions.get(c)?);
    let (na, nb, nc) = (ctx.mesh.normals.get(a)?, ctx.mesh.normals.get(b)?, ctx.mesh.normals.get(c)?);
    let x0 = (ua.x.min(ub.x).min(uc.x) * w as f32).floor().max(0.0) as u32;
    let x1 = ((ua.x.max(ub.x).max(uc.x) * w as f32).ceil().max(0.0) as u32).min(w);
    let y0 = (ua.y.min(ub.y).min(uc.y) * h as f32).floor().max(0.0) as u32;
    let y1 = ((ua.y.max(ub.y).max(uc.y) * h as f32).ceil().max(0.0) as u32).min(h);
    // Stamp radius in texels on this triangle: texel area over world area.
    let uv_area = (*ub - *ua).perp_dot(*uc - *ua).abs() * w as f32 * h as f32;
    let world_area = (*pb - *pa).cross(*pc - *pa).length();
    let shape = ctx.shape.at_radius(s.radius * (uv_area / world_area).sqrt());
    let uvp = [*ua * Vec2::new(w as f32, h as f32), *ub * Vec2::new(w as f32, h as f32), *uc * Vec2::new(w as f32, h as f32)];
    let mut shared = false;
    let mut texels = Vec::new();
    let mut center = ([0.0f32; 2], f32::MAX);
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * w + x) as usize;
            let owner = ctx.gbuf.tri.get(i).copied().unwrap_or(NO_TRI);
            let [b1, b2] = if owner == t {
                let Some(b) = ctx.gbuf.bary.get(i).copied() else { continue };
                b
            } else if owner != NO_TRI {
                // The texel is owned by another triangle mapped to the same UVs (mirrored or
                // instanced parts): this triangle's surface point there gets painted too.
                let Some(b) = uv_bary(uvp, [x as f32 + 0.5, y as f32 + 0.5]) else { continue };
                shared = true;
                b
            } else {
                continue;
            };
            let b0 = 1.0 - b1 - b2;
            let p = *pa * b0 + *pb * b1 + *pc * b2;
            let d = p - s.center;
            let lx = d.dot(s.tangent) / s.radius;
            let ly = d.dot(s.bitangent) / s.radius;
            if lx * lx + ly * ly > 2.0 {
                continue;
            }
            if d.dot(s.normal).abs() > s.radius * s.depth {
                continue;
            }
            let dist = d.length_squared();
            if dist < center.1 {
                center = ([x as f32 + 0.5, y as f32 + 0.5], dist);
            }
            let n = (*na * b0 + *nb * b1 + *nc * b2).try_normalize().unwrap_or(s.normal);
            let to_eye = (ctx.eye - p).try_normalize().unwrap_or(n);
            let facing = n.dot(to_eye);
            // Symmetry copies paint where the camera may not see (the back of the model).
            let culled = ctx.backface_culling && !s.copy;
            if culled && facing <= 0.0 {
                continue;
            }
            if n.dot(s.normal) <= 0.0 {
                continue;
            }
            if let Some(depth) = ctx.depth.filter(|_| !s.copy)
                && !depth.visible(p)
            {
                continue;
            }
            let fade = if culled { smoothstep(fade_lo, fade_hi, facing) } else { 1.0 };
            let a = shape.sample(lx, ly, s.hardness, s.seed) * s.flow * fade;
            if a > 0.0 {
                texels.push((x, y, a));
            }
        }
    }
    (center.1 < f32::MAX || !texels.is_empty()).then_some((texels, center, shared))
}

/// Barycentric weights (of vertices 1 and 2) of texel point `p` in the texel-space triangle
/// `uv`, edges included as in the G-buffer; None outside or for a degenerate triangle.
fn uv_bary(uv: [Vec2; 3], p: [f32; 2]) -> Option<[f32; 2]> {
    let edge = |a: Vec2, b: Vec2, p: Vec2| (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
    let [pa, pb, pc] = uv;
    let p = Vec2::from(p);
    let area = edge(pa, pb, pc);
    if area.abs() < 1e-12 {
        return None;
    }
    let s = area.signum();
    let (w0, w1, w2) = (edge(pb, pc, p) * s, edge(pc, pa, p) * s, edge(pa, pb, p) * s);
    let eps = -1e-6 * area.abs();
    if w0 < eps || w1 < eps || w2 < eps {
        return None;
    }
    let inv = 1.0 / area.abs();
    Some([w1 * inv, w2 * inv])
}

/// Stroke alpha covering whole triangles (polygon fill): every texel of `tris` gets alpha 1.
pub fn fill_triangles(canvas: &mut StrokeCanvas, gbuf: &UvGBuffer, tris: &BTreeSet<u32>) {
    fill_coverage(canvas, gbuf, |_, t| if tris.contains(&t) { 1.0 } else { 0.0 });
}

/// One-shot fill of the texture set's texels: `coverage(texel index, triangle)` gives each
/// covered texel's alpha (lasso and polygon fills).
pub fn fill_coverage(canvas: &mut StrokeCanvas, gbuf: &UvGBuffer, coverage: impl Fn(usize, u32) -> f32 + Sync) {
    let w = gbuf.width.max(1);
    canvas.fit(gbuf);
    canvas.texels = gbuf
        .tri
        .par_iter()
        .enumerate()
        .filter(|(_, t)| **t != NO_TRI)
        .filter_map(|(i, t)| {
            let a = coverage(i, *t).min(1.0);
            (a > 0.0).then_some(((i as u32) % w, (i as u32) / w, a))
        })
        .collect();
    canvas.add_stamp([0.0; 2]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use texturecraft_geom::primitives;

    #[test]
    fn one_stamp_on_a_plane_paints_a_disc() {
        let m = primitives::plane(2.0, 8);
        let bvh = Bvh::new(&m);
        let g = UvGBuffer::new(&m, 0, 128, 128);
        let mut canvas = StrokeCanvas::new(128, 128);
        let ctx = RasterCtx {
            mesh: &m,
            bvh: &bvh,
            gbuf: &g,
            material: 0,
            depth: None,
            eye: Vec3::new(0.0, 5.0, 0.0),
            backface_culling: true,
            angle_fade: (70.0, 85.0),
            shape: Shape { alpha: crate::BrushAlpha::Round, image: None, library: None, level: 0 },
        };
        let st = Stamp {
            center: Vec3::ZERO,
            normal: Vec3::Y,
            tangent: Vec3::X,
            bitangent: Vec3::Z,
            radius: 0.25,
            flow: 1.0,
            hardness: 0.9,
            depth: 1.0,
            seed: 0,
            copy: false,
        };
        rasterize(&mut canvas, &ctx, &[st]);
        // Plane is 2 units over 128 texels: radius 0.25 = 16 texels around the center (64, 64).
        let at = |x: u32, y: u32| from_u16(canvas.alpha.get(x, y)[0]);
        assert!(at(64, 64) > 0.99);
        assert!(at(64 + 12, 64) > 0.9);
        assert!(at(64 + 20, 64) < 0.01);
        assert!(!canvas.dirty.is_empty());

        // From below (backface), nothing is painted.
        let mut c2 = StrokeCanvas::new(128, 128);
        let below = RasterCtx { eye: Vec3::new(0.0, -5.0, 0.0), ..ctx };
        rasterize(&mut c2, &below, &[st]);
        assert!(c2.dirty.is_empty());
    }

    #[test]
    fn parts_sharing_uvs_are_both_paintable() {
        // Two planes with identical UVs, the second moved 10 units along Z.
        let p = primitives::plane(2.0, 8);
        let n = p.positions.len() as u32;
        let positions: Vec<Vec3> = p.positions.iter().copied().chain(p.positions.iter().map(|v| *v + Vec3::new(0.0, 0.0, 10.0))).collect();
        let uvs = p.uvs.iter().chain(p.uvs.iter()).copied().collect();
        let indices = p.indices.iter().copied().chain(p.indices.iter().map(|i| i + n)).collect();
        let m = Mesh::from_parts(positions, None, uvs, indices, "m").unwrap();
        let bvh = Bvh::new(&m);
        let g = UvGBuffer::new(&m, 0, 128, 128);
        assert!(g.overlaps > 0);
        let (lone, lone_bvh) = (primitives::plane(2.0, 8), Bvh::new(&p));
        let lone_g = UvGBuffer::new(&lone, 0, 128, 128);
        let stamp = |z: f32| Stamp {
            center: Vec3::new(0.0, 0.0, z),
            normal: Vec3::Y,
            tangent: Vec3::X,
            bitangent: Vec3::Z,
            radius: 0.25,
            flow: 1.0,
            hardness: 0.9,
            depth: 1.0,
            seed: 0,
            copy: false,
        };
        for z in [0.0, 10.0] {
            let ctx = RasterCtx {
                mesh: &m,
                bvh: &bvh,
                gbuf: &g,
                material: 0,
                depth: None,
                eye: Vec3::new(0.0, 5.0, z),
                backface_culling: true,
                angle_fade: (70.0, 85.0),
                shape: Shape { alpha: crate::BrushAlpha::Round, image: None, library: None, level: 0 },
            };
            let mut canvas = StrokeCanvas::new(128, 128);
            rasterize(&mut canvas, &ctx, &[stamp(z)]);
            let at = |x: u32, y: u32| from_u16(canvas.alpha.get(x, y)[0]);
            assert!(at(64, 64) > 0.99, "part at z={z}: {}", at(64, 64));
            assert!(at(64 + 12, 64) > 0.9, "part at z={z}: {}", at(64 + 12, 64));
            assert!(at(64 + 20, 64) < 0.01, "part at z={z}: {}", at(64 + 20, 64));
            // Same texels and alphas as painting a lone plane (no texel counted twice).
            let mut reference = StrokeCanvas::new(128, 128);
            let lone_ctx = RasterCtx { mesh: &lone, bvh: &lone_bvh, gbuf: &lone_g, eye: Vec3::new(0.0, 5.0, 0.0), ..ctx };
            rasterize(&mut reference, &lone_ctx, &[stamp(0.0)]);
            for y in 0..128 {
                for x in 0..128 {
                    assert_eq!(canvas.alpha.get(x, y), reference.alpha.get(x, y), "part at z={z}, texel ({x},{y})");
                }
            }
        }
    }
}

/// A dab in texture space (2D view painting): center and radius in UV units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UvStamp {
    pub center: texturecraft_geom::glam::Vec2,
    pub radius: f32,
    pub angle: f32,
    pub flow: f32,
    pub hardness: f32,
    pub seed: u32,
}

/// Accumulate UV-space stamps into `canvas` (only texels covered by the texture set).
pub fn rasterize_uv(canvas: &mut StrokeCanvas, gbuf: &UvGBuffer, shape: Shape, stamps: &[UvStamp]) {
    let (w, h) = (gbuf.width, gbuf.height);
    canvas.fit(gbuf);
    for s in stamps {
        if s.radius.is_nan() || s.radius <= 0.0 || !s.center.is_finite() {
            continue;
        }
        let (cx, cy, r) = (s.center.x * w as f32, s.center.y * h as f32, s.radius * w as f32);
        let x0 = (cx - r * 1.42).floor().max(0.0) as u32;
        let x1 = ((cx + r * 1.42).ceil().max(0.0) as u32).min(w);
        let y0 = (cy - r * 1.42).floor().max(0.0) as u32;
        let y1 = ((cy + r * 1.42).ceil().max(0.0) as u32).min(h);
        let (sn, cs) = s.angle.to_radians().sin_cos();
        let shape = shape.at_radius(r);
        for y in y0..y1 {
            for x in x0..x1 {
                if gbuf.tri.get((y * w + x) as usize).copied().unwrap_or(NO_TRI) == NO_TRI {
                    continue;
                }
                // Local frame with y up on screen (texel rows grow downward), as in the 3D view.
                let (dx, dy) = ((x as f32 + 0.5 - cx) / r, (cy - y as f32 - 0.5) / r);
                let (lx, ly) = (dx * cs + dy * sn, -dx * sn + dy * cs);
                let a = shape.sample(lx, ly, s.hardness, s.seed) * s.flow;
                if a > 0.0 {
                    canvas.texels.push((x, y, a));
                }
            }
        }
        canvas.add_stamp([cx, cy]);
    }
}
