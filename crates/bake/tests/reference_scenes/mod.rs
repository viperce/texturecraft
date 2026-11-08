//! Analytic reference scenes for the mesh-map baker.
//!
//! Each scene is a small mesh built in code whose correct map values follow in closed form, so a
//! check fails when the baker is wrong rather than when it changes. The checks take the ray
//! tracer as a parameter: `tests/reference.rs` runs them on the CPU tracer, and the render
//! crate's `gpu_reference` tests include this file to run the same expectations on the GPU.
//!
//! Secondary-ray maps are Monte Carlo estimates. Their tolerances are four standard deviations
//! of the mean, from a bound on the per-ray variance and the number of rays averaged: `rays` per
//! texel (antialiasing may add more, never fewer) times the texels averaged. The ray pattern is
//! a rotated Hammersley set seeded by the texel, so runs are deterministic and the real error is
//! well inside these bounds.

use std::f32::consts::PI;

use texturecraft_bake::tracing::Tracer;
use texturecraft_bake::{BakeOutput, BakeRequest, HighPoly, Progress, bake_on};
use texturecraft_core::{
    AmbientOcclusionAttenuation, BackfaceMode, BakeAntialiasing, BakeResolution, BakeSettings, BakedMap, HeightNormalization, RayDistribution, RaySettings,
    SelfOcclusion, ThicknessNormalization,
};
use texturecraft_geom::glam::{Vec2, Vec3};
use texturecraft_geom::{Bvh, Mesh, Surface, UvGBuffer, primitives, surface_at};

/// A reference check, run on the given tracer
pub type Check = fn(&dyn Tracer);

/// Every check, by name, for the tracer-parameterized runners.
pub const CHECKS: [(&str, Check); 12] = [
    ("occlusion_beside_a_wall", occlusion_beside_a_wall),
    ("occlusion_inside_an_open_box", occlusion_inside_an_open_box),
    ("height_of_a_step", height_of_a_step),
    ("height_of_a_spherical_cap", height_of_a_spherical_cap),
    ("tangent_normal_of_a_tilted_plane", tangent_normal_of_a_tilted_plane),
    ("world_normal_and_position", world_normal_and_position),
    ("thickness_of_a_slab", thickness_of_a_slab),
    ("opacity_of_a_hole", opacity_of_a_hole),
    ("normal_map_round_trip", normal_map_round_trip),
    ("normal_map_round_trip_with_mirrored_uvs", normal_map_round_trip_with_mirrored_uvs),
    ("padding_fills_each_gutter_from_the_nearest_island", padding_fills_each_gutter_from_the_nearest_island),
    ("tangent_normals_agree_across_a_uv_seam", tangent_normals_agree_across_a_uv_seam),
];

// ---------------------------------------------------------------------------------------------
// Baking and sampling helpers

fn settings(resolution: u32, antialiasing: BakeAntialiasing, maps: &[BakedMap]) -> BakeSettings {
    let mut settings = BakeSettings::default();
    settings.common.resolution = BakeResolution::from_pixels(resolution).unwrap();
    settings.common.antialiasing = antialiasing;
    settings.common.dilation = 0;
    settings.common.apply_diffusion = false;
    settings.maps.select(maps.iter().copied());
    settings
}

/// Secondary rays in mesh units from the surface itself (no minimum distance)
fn rays(count: u32, max_distance: f32, spread: f32, distribution: RayDistribution) -> RaySettings {
    RaySettings { rays: count, min_distance: 0.0, max_distance, relative_to_bounding_box: false, spread, distribution }
}

/// A transfer cage in mesh units
fn cage(settings: &mut BakeSettings, frontal: f32, rear: f32) {
    settings.cage.max_frontal = frontal;
    settings.cage.max_rear = rear;
    settings.cage.relative_to_bounding_box = false;
}

fn bake_with(tracer: &dyn Tracer, low: &Mesh, high: Option<&Mesh>, settings: &BakeSettings) -> BakeOutput {
    let low_bvh = Bvh::new(low);
    let high_bvh = high.map(Bvh::new);
    let high = high.zip(high_bvh.as_ref()).map(|(mesh, bvh)| HighPoly { file_ids: &[], mesh, bvh, base_color: &[] });
    let request =
        BakeRequest { low_base_color: &[], mesh: low, bvh: &low_bvh, material: 0, settings, high, cage: None, skew: None, cage_paint: None, tiles: (1, 1) };
    let output = bake_on(&request, &Progress::new(), tracer).unwrap();
    assert!(!output.tracer.contains("unavailable"), "the bake fell back to the CPU: {}", output.tracer);
    output
}

/// A covered texel and the low-poly surface under each of its covered subsamples. The bake
/// averages a texel's covered subsamples, so its expected value is the mean of the analytic
/// value over these surfaces.
struct Texel {
    index: usize,
    x: usize,
    surfaces: Vec<Surface>,
}

impl Texel {
    fn mean(&self, value: impl Fn(&Surface) -> f32) -> f32 {
        self.surfaces.iter().map(value).sum::<f32>() / self.surfaces.len() as f32
    }

    fn mean_vector(&self, value: impl Fn(&Surface) -> Vec3) -> Vec3 {
        self.surfaces.iter().map(value).sum::<Vec3>() / self.surfaces.len() as f32
    }
}

fn texels(mesh: &Mesh, resolution: u32, antialiasing: BakeAntialiasing) -> Vec<Texel> {
    let (size, per_axis) = (resolution as usize, antialiasing.samples_per_axis() as usize);
    let fine = UvGBuffer::new(mesh, 0, (size * per_axis) as u32, (size * per_axis) as u32);
    (0..size * size)
        .filter_map(|index| {
            let (x, y) = (index % size, index / size);
            let surfaces: Vec<Surface> = (0..per_axis * per_axis)
                .filter_map(|k| fine.surface(mesh, (y * per_axis + k / per_axis) * size * per_axis + x * per_axis + k % per_axis))
                .collect();
            (!surfaces.is_empty()).then_some(Texel { index, x, surfaces })
        })
        .collect()
}

fn scalar(output: &BakeOutput, map: BakedMap, index: usize) -> f32 {
    output.maps[&map][index * map.components()]
}

fn vector(output: &BakeOutput, map: BakedMap, index: usize) -> Vec3 {
    Vec3::from_slice(&output.maps[&map][index * 3..index * 3 + 3])
}

fn unpack(packed: Vec3) -> Vec3 {
    packed * 2.0 - Vec3::ONE
}

fn pack(normal: Vec3) -> Vec3 {
    normal * 0.5 + Vec3::splat(0.5)
}

fn degrees_between(a: Vec3, b: Vec3) -> f32 {
    a.normalize().dot(b.normalize()).clamp(-1.0, 1.0).acos().to_degrees()
}

/// Four standard deviations of a mean over `samples` draws whose variance is at most `variance`
fn four_sigma(variance: f32, samples: usize) -> f32 {
    4.0 * (variance / samples as f32).sqrt()
}

/// Bilinear sample of a row-major `resolution²` map with texel centres at `(i + 0.5) / resolution`
fn bilinear(output: &BakeOutput, map: BakedMap, uv: Vec2) -> Vec3 {
    let size = output.resolution as usize;
    let texel = |x: isize, y: isize| {
        let (x, y) = (x.clamp(0, size as isize - 1) as usize, y.clamp(0, size as isize - 1) as usize);
        vector(output, map, y * size + x)
    };
    let (fx, fy) = (uv.x * size as f32 - 0.5, uv.y * size as f32 - 0.5);
    let (x, y) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x, fy - y);
    let (x, y) = (x as isize, y as isize);
    texel(x, y).lerp(texel(x + 1, y), tx).lerp(texel(x, y + 1).lerp(texel(x + 1, y + 1), tx), ty)
}

// ---------------------------------------------------------------------------------------------
// Mesh builders

/// One quad with a flat normal, wound so its geometric normal agrees with `normal`
fn face(corners: [Vec3; 4], normal: Vec3, uvs: [Vec2; 4], name: &str) -> Mesh {
    let facing = (corners[1] - corners[0]).cross(corners[2] - corners[0]).dot(normal) > 0.0;
    let indices = if facing { vec![0, 1, 2, 0, 2, 3] } else { vec![0, 2, 1, 0, 3, 2] };
    Mesh::from_parts(corners.to_vec(), Some(vec![normal; 4]), uvs.to_vec(), indices, name).unwrap()
}

/// A horizontal rectangle `x0..x1 × z0..z1` at height `y`, facing up, UVs from `uv(x, z)`
fn floor(x: [f32; 2], z: [f32; 2], y: f32, uv: impl Fn(f32, f32) -> Vec2) -> Mesh {
    let corners = [Vec3::new(x[0], y, z[0]), Vec3::new(x[1], y, z[0]), Vec3::new(x[1], y, z[1]), Vec3::new(x[0], y, z[1])];
    face(corners, Vec3::Y, corners.map(|c| uv(c.x, c.z)), "Floor")
}

/// The flat low poly of the transfer scenes: 2 × 2 at y = 0, UV (x, z) → ((x + 1) / 2, (z + 1) / 2)
fn low_quad() -> Mesh {
    floor([-1.0, 1.0], [-1.0, 1.0], 0.0, |x, z| Vec2::new((x + 1.0) * 0.5, (z + 1.0) * 0.5))
}

/// A `cells²` grid over `-half..half` at heights `height(x, z)`, with smooth normals
fn heightfield(half: f32, cells: usize, height: impl Fn(f32, f32) -> f32) -> Mesh {
    let mut mesh = primitives::plane(2.0 * half, cells);
    for p in &mut mesh.positions {
        p.y = height(p.x, p.z);
    }
    mesh.compute_normals();
    mesh.compute_tangents();
    mesh
}

// ---------------------------------------------------------------------------------------------
// Ambient occlusion and bent normals

const WALL_HEIGHT: f32 = 1.0;
const WALL_HALF_LENGTH: f32 = 50.0;

/// A floor `x ∈ -2..0`, `z ∈ -0.5..0.5` (the baked material) against a wall at `x = 0`, one unit
/// high and `±WALL_HALF_LENGTH` long, facing the floor (material 1, not baked).
fn floor_beside_a_wall() -> Mesh {
    let mut mesh = floor([-2.0, 0.0], [-0.5, 0.5], 0.0, |x, z| Vec2::new((x + 2.0) * 0.5, z + 0.5));
    let (h, l) = (WALL_HEIGHT, WALL_HALF_LENGTH);
    mesh.append(&face([Vec3::new(0.0, 0.0, -l), Vec3::new(0.0, 0.0, l), Vec3::new(0.0, h, l), Vec3::new(0.0, h, -l)], Vec3::NEG_X, [Vec2::ZERO; 4], "Wall"));
    mesh
}

/// Occlusion maps with every filter off, so a ray counts as occluded exactly when it meets a
/// triangle within `max_distance`
fn occlusion_settings(resolution: u32, antialiasing: BakeAntialiasing, rays: RaySettings) -> BakeSettings {
    let mut settings = settings(resolution, antialiasing, &[BakedMap::AmbientOcclusion, BakedMap::BentNormal]);
    let ao = &mut settings.maps.ao.parameters;
    ao.rays = rays;
    ao.attenuation = AmbientOcclusionAttenuation::None;
    ao.ignore_backface = BackfaceMode::Never;
    ao.self_occlusion = SelfOcclusion::Always;
    ao.ground_plane.enabled = false;
    let bent = &mut settings.maps.bent_normal.parameters;
    bent.rays = rays;
    bent.ignore_backface = BackfaceMode::Never;
    bent.self_occlusion = SelfOcclusion::Always;
    settings
}

/// Open directions above a floor point at distance `d` from an infinitely long wall of height
/// `h`. The wall hides the lune between the floor and the plane through the wall's top edge and
/// the point's z axis, of dihedral angle `β = atan(h / d)` (β = π/2 at the base: half the
/// hemisphere). With `ω = (r cos φ, r sin φ, z)`, `r = √(1 − z²)`, `dω = dφ dz` and the wall in
/// `φ ∈ (0, β)`:
/// - uniform rays: AO = 1 − 2β / 2π = 1 − β/π; mean open direction ∫ ω dω / 2π over `φ ∈ (β, π)`
///   = ¼ (−sin β, 1 + cos β, 0), which leans β/2 away from the wall;
/// - cosine rays (pdf cos θ / π, cos θ = r sin φ): AO = 1 − (1 − cos β)/2 = (1 + cos β)/2, the
///   classic view factor; mean open direction ∫ ω cos θ dω / π = (1/π)(−⅔ sin²β, ⅔(π − β) + ⅓ sin 2β, 0).
///
/// Returns (AO, mean open direction). The bent normal is that direction normalized.
fn wall_expectation(d: f32, h: f32, distribution: RayDistribution) -> (f32, Vec3) {
    let beta = h.atan2(d);
    match distribution {
        RayDistribution::Uniform => (1.0 - beta / PI, Vec3::new(-beta.sin(), 1.0 + beta.cos(), 0.0) * 0.25),
        RayDistribution::Cosine => {
            ((1.0 + beta.cos()) * 0.5, Vec3::new(-2.0 / 3.0 * beta.sin().powi(2), 2.0 / 3.0 * (PI - beta) + (2.0 * beta).sin() / 3.0, 0.0) / PI)
        }
    }
}

/// Mean measured and expected AO, and the angle between the measured and expected bent normals
/// (both averaged over the group), for a group of texels.
fn compare_occlusion(output: &BakeOutput, group: &[&Texel], expected: impl Fn(&Surface) -> (f32, Vec3)) -> (f32, f32, f32, f32) {
    let count = group.len() as f32;
    let measured_ao = group.iter().map(|t| scalar(output, BakedMap::AmbientOcclusion, t.index)).sum::<f32>() / count;
    let expected_ao = group.iter().map(|t| t.mean(|s| expected(s).0)).sum::<f32>() / count;
    let measured_bent = group.iter().map(|t| unpack(vector(output, BakedMap::BentNormal, t.index))).sum::<Vec3>();
    let expected_bent = group.iter().map(|t| t.mean_vector(|s| expected(s).1.normalize())).sum::<Vec3>();
    // The smallest mean open direction in the group bounds the bent-normal noise
    let moment = group.iter().flat_map(|t| t.surfaces.iter().map(|s| expected(s).1.length())).fold(f32::INFINITY, f32::min);
    (measured_ao, expected_ao, degrees_between(measured_bent, expected_bent), moment)
}

/// AO and bent normals on a floor beside a wall match the lune solid angle and view factor.
pub fn occlusion_beside_a_wall(tracer: &dyn Tracer) {
    const RAYS: u32 = 256;
    let mesh = floor_beside_a_wall();
    for antialiasing in [BakeAntialiasing::None, BakeAntialiasing::Subsampling2x2] {
        for distribution in [RayDistribution::Uniform, RayDistribution::Cosine] {
            // Rays long enough to reach the wall from anywhere on the floor (at most √(2² + 1² + 50²))
            let output = bake_with(tracer, &mesh, None, &occlusion_settings(64, antialiasing, rays(RAYS, 1000.0, 180.0, distribution)));
            let texels = texels(&mesh, 64, antialiasing);
            // Eight bands of eight columns, from the far edge (d ≈ 2) to the wall's base (d ≈ 0)
            for band in 0..8 {
                let group: Vec<&Texel> = texels.iter().filter(|t| t.x / 8 == band).collect();
                let (measured, expected, bent_error, moment) = compare_occlusion(&output, &group, |s| wall_expectation(-s.pos.x, WALL_HEIGHT, distribution));
                // An occlusion ray is Bernoulli(p): variance p(1 − p) ≤ ¼. The finite wall misses
                // rays within d/L of its ends; for d ≤ 2, L = 50 that is ≤ dh/(2πL²) < 2e-4 of the
                // hemisphere, so 1e-3 covers it.
                let tolerance = four_sigma(expected * (1.0 - expected), RAYS as usize * group.len()) + 1e-3;
                // Each component of a ray direction has variance ≤ 1, and the bent normal is the
                // mean open direction (length ≥ `moment`) normalized: two transverse components
                // give an angular error ≤ √2 σ / |mean|.
                let bent_tolerance = (four_sigma(2.0, RAYS as usize * group.len()) / moment).to_degrees();
                eprintln!(
                    "wall {antialiasing:?} {distribution:?} band {band}: AO {measured:.4} vs {expected:.4} (±{tolerance:.4}), bent off by {bent_error:.2}° (±{bent_tolerance:.2}°)"
                );
                assert!((measured - expected).abs() < tolerance, "wall {antialiasing:?} {distribution:?} band {band}: AO {measured} vs {expected}");
                assert!(bent_error < bent_tolerance, "wall {antialiasing:?} {distribution:?} band {band}: bent normal off by {bent_error}°");
            }
        }
    }
}

/// Floor `-1..1` square (baked) inside four walls of height `BOX_HEIGHT` facing inwards, no lid
const BOX_HEIGHT: f32 = 1.0;

fn open_box() -> Mesh {
    let mut mesh = floor([-1.0, 1.0], [-1.0, 1.0], 0.0, |x, z| Vec2::new((x + 1.0) * 0.5, (z + 1.0) * 0.5));
    let h = BOX_HEIGHT;
    for (inward, along) in [(Vec3::X, Vec3::Z), (Vec3::NEG_X, Vec3::Z), (Vec3::Z, Vec3::X), (Vec3::NEG_Z, Vec3::X)] {
        let base = -inward;
        let corners = [base - along, base + along, base + along + Vec3::Y * h, base - along + Vec3::Y * h];
        mesh.append(&face(corners, inward, [Vec2::ZERO; 4], "Wall"));
    }
    mesh
}

/// Solid angle of a `a × b` rectangle parallel to the floor at height `c`, with a corner straight
/// above the point: `atan(ab / (c √(a² + b² + c²)))`.
fn corner_solid_angle(a: f32, b: f32, c: f32) -> f32 {
    (a * b / (c * (a * a + b * b + c * c).sqrt())).atan()
}

/// View factor from an upward-facing point to the same rectangle (Howell's catalogue, B-2):
/// `(1/2π) [X/√(1+X²) atan(Y/√(1+X²)) + Y/√(1+Y²) atan(X/√(1+Y²))]` with `X = a/c`, `Y = b/c`.
fn corner_view_factor(a: f32, b: f32, c: f32) -> f32 {
    let (x, y) = (a / c, b / c);
    let (sx, sy) = ((1.0 + x * x).sqrt(), (1.0 + y * y).sqrt());
    (x / sx * (y / sx).atan() + y / sy * (x / sy).atan()) / (2.0 * PI)
}

/// `∫ ω dω` over a convex spherical polygon seen from `p` (Arvo's vector irradiance:
/// ½ Σ Θᵢ Γᵢ over the edges, Θᵢ the angle an edge subtends and Γᵢ the unit normal of the plane
/// through `p` and that edge), oriented into the polygon.
fn polygon_moment(p: Vec3, corners: &[Vec3]) -> Vec3 {
    let directions: Vec<Vec3> = corners.iter().map(|c| (*c - p).normalize()).collect();
    let sum: Vec3 =
        directions.iter().zip(directions.iter().cycle().skip(1)).map(|(a, b)| a.cross(*b).normalize() * a.dot(*b).clamp(-1.0, 1.0).acos()).sum::<Vec3>() * 0.5;
    let centre = corners.iter().copied().sum::<Vec3>() / corners.len() as f32;
    if sum.dot(centre - p) < 0.0 { -sum } else { sum }
}

/// The open directions from a floor point are exactly those through the box's opening, a
/// rectangle at height h: the sum over the four rectangles with a corner above the point of
/// their solid angle (uniform AO = Ω/2π) or view factor (cosine AO). The uniform bent normal is
/// the opening's mean direction (`polygon_moment`), divided by 2π for the mean open direction.
fn box_expectation(p: Vec3, distribution: RayDistribution) -> (f32, Vec3) {
    let c = BOX_HEIGHT - p.y;
    let quadrants = [(1.0 - p.x, 1.0 - p.z), (1.0 + p.x, 1.0 - p.z), (1.0 - p.x, 1.0 + p.z), (1.0 + p.x, 1.0 + p.z)];
    let opening = [Vec3::new(-1.0, BOX_HEIGHT, -1.0), Vec3::new(1.0, BOX_HEIGHT, -1.0), Vec3::new(1.0, BOX_HEIGHT, 1.0), Vec3::new(-1.0, BOX_HEIGHT, 1.0)];
    let moment = polygon_moment(p, &opening) / (2.0 * PI);
    match distribution {
        RayDistribution::Uniform => (quadrants.iter().map(|(a, b)| corner_solid_angle(*a, *b, c)).sum::<f32>() / (2.0 * PI), moment),
        RayDistribution::Cosine => (quadrants.iter().map(|(a, b)| corner_view_factor(*a, *b, c)).sum(), moment),
    }
}

/// AO inside an open box matches the solid angle and view factor of its opening; the uniform
/// bent normal points at the opening's mean direction.
pub fn occlusion_inside_an_open_box(tracer: &dyn Tracer) {
    const RAYS: u32 = 256;
    let mesh = open_box();
    // At the centre of a unit-high box over a 2 × 2 floor: Ω = 4 · atan(1/√3) = 2π/3
    assert!((box_expectation(Vec3::ZERO, RayDistribution::Uniform).0 - 1.0 / 3.0).abs() < 1e-6);
    assert!(box_expectation(Vec3::ZERO, RayDistribution::Uniform).1.normalize().abs_diff_eq(Vec3::Y, 1e-6));
    for distribution in [RayDistribution::Uniform, RayDistribution::Cosine] {
        let antialiasing = BakeAntialiasing::Subsampling2x2;
        // Rays long enough to reach any wall: the floor's corner to the opposite top edge is 3 long
        let output = bake_with(tracer, &mesh, None, &occlusion_settings(32, antialiasing, rays(RAYS, 10.0, 180.0, distribution)));
        let texels = texels(&mesh, 32, antialiasing);
        // Four rings by distance from the centre
        for ring in 0..4 {
            let group: Vec<&Texel> = texels.iter().filter(|t| (t.surfaces[0].pos.length() / 0.36) as usize == ring).collect();
            let (measured, expected, bent_error, moment) = compare_occlusion(&output, &group, |s| box_expectation(s.pos, distribution));
            // Bernoulli rays as for the wall; the scene is closed apart from the opening, so the
            // expectation is exact
            let tolerance = four_sigma(expected * (1.0 - expected), RAYS as usize * group.len());
            let bent_tolerance = (four_sigma(2.0, RAYS as usize * group.len()) / moment).to_degrees();
            eprintln!(
                "open box {distribution:?} ring {ring} ({} texels): AO {measured:.4} vs {expected:.4} (±{tolerance:.4}), bent off by {bent_error:.2}°",
                group.len()
            );
            assert!((measured - expected).abs() < tolerance, "open box {distribution:?} ring {ring}: AO {measured} vs {expected}");
            if distribution == RayDistribution::Uniform {
                assert!(bent_error < bent_tolerance, "open box ring {ring}: bent normal off by {bent_error}° (±{bent_tolerance}°)");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Height

fn height_settings(resolution: u32, normalization: HeightNormalization) -> BakeSettings {
    let mut settings = settings(resolution, BakeAntialiasing::None, &[BakedMap::Height]);
    settings.maps.height.parameters.normalization = normalization;
    settings.maps.height.parameters.scaling_divisor = 1.0;
    settings
}

/// A high poly that is two flat steps over the flat low poly reads as their exact offsets: +0.05
/// for `x < 0`, −0.03 for `x > 0`.
pub fn height_of_a_step(tracer: &dyn Tracer) {
    let low = low_quad();
    let mut high = floor([-1.5, 0.0], [-1.5, 1.5], 0.05, |_, _| Vec2::ZERO);
    high.append(&floor([0.0, 1.5], [-1.5, 1.5], -0.03, |_, _| Vec2::ZERO));
    let texels = texels(&low, 32, BakeAntialiasing::None);
    let offset = |t: &Texel| if t.surfaces[0].pos.x < 0.0 { 0.05 } else { -0.03 };
    // Manual with divisor 1: 0.5 + h. Ray distances (frontal = rear = 0.1): 0.5 + h / 0.2.
    // Min/max: the two steps are the extremes, 1 and 0.
    let cases: [(HeightNormalization, &dyn Fn(f32) -> f32); 3] = [
        (HeightNormalization::Manual, &|h| 0.5 + h),
        (HeightNormalization::RelativeToRayDistances, &|h| 0.5 + h / 0.2),
        (HeightNormalization::RelativeToMinMax, &|h| if h > 0.0 { 1.0 } else { 0.0 }),
    ];
    for (normalization, expected) in cases {
        let mut settings = height_settings(32, normalization);
        cage(&mut settings, 0.1, 0.1);
        let output = bake_with(tracer, &low, Some(&high), &settings);
        let worst = texels.iter().map(|t| (scalar(&output, BakedMap::Height, t.index) - expected(offset(t))).abs()).fold(0.0, f32::max);
        eprintln!("step height {normalization:?}: worst error {worst:.2e}");
        // Exact up to single-precision ray distances
        assert!(worst < 1e-5, "step height {normalization:?}: off by {worst}");
    }
}

/// A spherical cap of radius 1 rising 0.2 above the flat low poly reads as its analytic height
/// `√(1 − r²) − 0.8` inside the rim (r < 0.6), 0 outside.
pub fn height_of_a_spherical_cap(tracer: &dyn Tracer) {
    const CELL: f32 = 0.01;
    let cap = |x: f32, z: f32| ((1.0 - x * x - z * z).max(0.0).sqrt() - 0.8).max(0.0);
    let rim = 0.6f32;
    let low = low_quad();
    let high = heightfield(1.2, 240, cap);
    let mut settings = height_settings(128, HeightNormalization::Manual);
    cage(&mut settings, 0.3, 0.1);
    let output = bake_with(tracer, &low, Some(&high), &settings);
    // The high poly interpolates the cap linearly over cells of 0.01. Away from the rim the
    // error is at most ½ h² max|∇²f| = ½ · 1e-4 · 1.9 < 1e-4 (|f''| = 1/(1 − r²)^{3/2} ≤ 1.9
    // for r ≤ 0.58); within two cells of the rim the kink in the cap breaks that bound.
    let mut worst = 0.0f32;
    for texel in texels(&low, 128, BakeAntialiasing::None) {
        let p = texel.surfaces[0].pos;
        if ((p.x * p.x + p.z * p.z).sqrt() - rim).abs() < 2.0 * CELL {
            continue;
        }
        worst = worst.max((scalar(&output, BakedMap::Height, texel.index) - (0.5 + cap(p.x, p.z))).abs());
    }
    eprintln!("spherical cap height: worst error {worst:.2e}");
    assert!(worst < 2e-4, "spherical cap height off by {worst}");
}

// ---------------------------------------------------------------------------------------------
// Tangent-space and world-space normals, position

/// A plane through the origin with normal `n`, as a high poly covering the low quad
fn tilted_plane(n: Vec3) -> Mesh {
    let corners = [(-1.5, -1.5), (1.5, -1.5), (1.5, 1.5), (-1.5, 1.5)].map(|(x, z)| Vec3::new(x, -(n.x * x + n.z * z) / n.y, z));
    face(corners, n, [Vec2::ZERO; 4], "Tilted")
}

/// Over a flat low poly, a tilted plane has the tangent-space normal `(n·T, n·B, n·N)`, with T
/// along increasing u and B up the image (decreasing v: UVs are glTF, v down), OpenGL style.
/// Checked for a plain, a mirrored and a rotated UV layout, which give T and B the frames below.
pub fn tangent_normal_of_a_tilted_plane(tracer: &dyn Tracer) {
    let n = Vec3::new(0.3, 1.0, -0.4).normalize();
    let high = tilted_plane(n);
    // (name, UV of (x, z), T, B)
    type Layout = (&'static str, fn(f32, f32) -> Vec2, Vec3, Vec3);
    let layouts: [Layout; 3] = [
        ("plain", |x, z| Vec2::new((x + 1.0) * 0.5, (z + 1.0) * 0.5), Vec3::X, Vec3::NEG_Z),
        ("mirrored", |x, z| Vec2::new((1.0 - x) * 0.5, (z + 1.0) * 0.5), Vec3::NEG_X, Vec3::NEG_Z),
        ("rotated", |x, z| Vec2::new((z + 1.0) * 0.5, (1.0 - x) * 0.5), Vec3::Z, Vec3::X),
    ];
    for (name, uv, tangent, bitangent) in layouts {
        let low = floor([-1.0, 1.0], [-1.0, 1.0], 0.0, uv);
        let mut settings = settings(32, BakeAntialiasing::None, &[BakedMap::Normal, BakedMap::WorldNormal]);
        // The plane rises at most 0.7 / n.y above or below the quad's corners
        cage(&mut settings, 1.0, 1.0);
        let output = bake_with(tracer, &low, Some(&high), &settings);
        let expected = pack(Vec3::new(n.dot(tangent), n.dot(bitangent), n.dot(Vec3::Y)));
        let texels = texels(&low, 32, BakeAntialiasing::None);
        let worst = texels.iter().map(|t| (vector(&output, BakedMap::Normal, t.index) - expected).abs().max_element()).fold(0.0, f32::max);
        let worst_world = texels.iter().map(|t| (vector(&output, BakedMap::WorldNormal, t.index) - pack(n)).abs().max_element()).fold(0.0, f32::max);
        eprintln!("tilted plane, {name} UVs: expected {expected:?}, worst error {worst:.2e}, world normal {worst_world:.2e}");
        assert!(worst < 1e-5, "{name} UVs: tangent-space normal off by {worst} (expected {expected:?})");
        assert!(worst_world < 1e-5, "{name} UVs: world normal off by {worst_world}");
    }
}

/// Without a high poly, the world normal is the low poly's interpolated normal and the position
/// its bounding-box-normalized coordinates.
pub fn world_normal_and_position(tracer: &dyn Tracer) {
    let low = primitives::sphere(1.0, 32, 16);
    let mut settings = settings(64, BakeAntialiasing::Subsampling2x2, &[BakedMap::WorldNormal, BakedMap::Position]);
    settings.maps.position.parameters.normalization = texturecraft_core::PositionMode::BoundingBox;
    let output = bake_with(tracer, &low, None, &settings);
    let (minimum, maximum) = low.bounds();
    let (mut normal_error, mut position_error) = (0.0f32, 0.0f32);
    for texel in texels(&low, 64, BakeAntialiasing::Subsampling2x2) {
        let normal = texel.mean_vector(|s| pack(s.nrm));
        let position = texel.mean_vector(|s| (s.pos - minimum) / (maximum - minimum));
        normal_error = normal_error.max((vector(&output, BakedMap::WorldNormal, texel.index) - normal).abs().max_element());
        position_error = position_error.max((vector(&output, BakedMap::Position, texel.index) - position).abs().max_element());
    }
    eprintln!("world normal worst error {normal_error:.2e}, position {position_error:.2e}");
    assert!(normal_error < 1e-5 && position_error < 1e-5, "world normal {normal_error}, position {position_error}");
}

// ---------------------------------------------------------------------------------------------
// Thickness

/// The top face of a slab of thickness T = 0.2 that is wide enough for no ray to leave through
/// its sides. A ray at angle θ from the inward normal travels `min(T / cos θ, D)` to the bottom
/// face, D the maximum distance. With `c = cos θ` and the cone `c ≥ C`:
/// - uniform rays, c uniform on [C, 1]: `E = T ln(1/C) / (1 − C)`; for the hemisphere with the
///   cap, `E = T (1 + ln(D/T))`;
/// - cosine rays, pdf 2c / (1 − C²): `E = 2T / (1 + C)`; for the hemisphere with the cap,
///   `E = 2T − T²/D`.
pub fn thickness_of_a_slab(tracer: &dyn Tracer) {
    const T: f32 = 0.2;
    const RAYS: u32 = 128;
    let slab = primitives::rounded_box(Vec3::new(4.0, T * 0.5, 4.0), 0.0);
    let cone = (60.0f32).to_radians().cos();
    let cases = [
        (120.0, RayDistribution::Uniform, 10.0, T * (1.0 / cone).ln() / (1.0 - cone)),
        (120.0, RayDistribution::Cosine, 10.0, 2.0 * T / (1.0 + cone)),
        (180.0, RayDistribution::Uniform, 1.0, T * (1.0 + (1.0 / T).ln())),
        (180.0, RayDistribution::Cosine, 1.0, 2.0 * T - T * T),
    ];
    let texels = texels(&slab, 64, BakeAntialiasing::None);
    // The top face, at least 1.5 from its sides: rays reach at most 1 sideways
    let top: Vec<&Texel> = texels.iter().filter(|t| t.surfaces[0].nrm.y > 0.99 && t.surfaces[0].pos.x.abs().max(t.surfaces[0].pos.z.abs()) < 2.5).collect();
    for (spread, distribution, max_distance, expected) in cases {
        let mut settings = settings(64, BakeAntialiasing::None, &[BakedMap::Thickness]);
        settings.maps.thickness.parameters.rays = rays(RAYS, max_distance, spread, distribution);
        settings.maps.thickness.parameters.normalization = ThicknessNormalization::None;
        settings.maps.thickness.parameters.self_occlusion = SelfOcclusion::Always;
        let output = bake_with(tracer, &slab, None, &settings);
        let measured = top.iter().map(|t| scalar(&output, BakedMap::Thickness, t.index)).sum::<f32>() / top.len() as f32;
        // A ray's distance lies in [T, min(T / C, D)]: variance ≤ (range / 2)²
        let range = (T / cone.max(T / max_distance)).min(max_distance) - T;
        let tolerance = four_sigma((range * 0.5).powi(2), RAYS as usize * top.len());
        eprintln!("slab thickness, {spread}° {distribution:?}: {measured:.4} vs {expected:.4} (±{tolerance:.4}) over {} texels", top.len());
        assert!((measured - expected).abs() < tolerance, "slab thickness {spread}° {distribution:?}: {measured} vs {expected}");
    }
}

// ---------------------------------------------------------------------------------------------
// Opacity

/// A high poly with a rectangular hole: opacity is 0 where the transfer ray passes through the
/// hole and 1 elsewhere. The hole's edges sit on quarter texels, between the 4 × 4 subsamples,
/// so each antialiased texel is exactly the share of its subsamples outside the hole.
pub fn opacity_of_a_hole(tracer: &dyn Tracer) {
    let texel = 2.0 / 32.0;
    let (x0, x1, z0, z1) = (-0.5 + 0.25 * texel, 0.25 + 0.5 * texel, -0.25 + 0.75 * texel, 0.5 - 0.25 * texel);
    let y = 0.01;
    let mut high = floor([-1.5, x0], [-1.5, 1.5], y, |_, _| Vec2::ZERO);
    high.append(&floor([x1, 1.5], [-1.5, 1.5], y, |_, _| Vec2::ZERO));
    high.append(&floor([x0, x1], [-1.5, z0], y, |_, _| Vec2::ZERO));
    high.append(&floor([x0, x1], [z1, 1.5], y, |_, _| Vec2::ZERO));
    let low = low_quad();
    for antialiasing in [BakeAntialiasing::None, BakeAntialiasing::Subsampling4x4] {
        let mut settings = settings(32, antialiasing, &[BakedMap::Opacity]);
        cage(&mut settings, 0.05, 0.05);
        let output = bake_with(tracer, &low, Some(&high), &settings);
        let inside = |s: &Surface| (x0..x1).contains(&s.pos.x) && (z0..z1).contains(&s.pos.z);
        let texels = texels(&low, 32, antialiasing);
        let worst =
            texels.iter().map(|t| (scalar(&output, BakedMap::Opacity, t.index) - t.mean(|s| if inside(s) { 0.0 } else { 1.0 })).abs()).fold(0.0, f32::max);
        let partial = texels.iter().filter(|t| (0.01..0.99).contains(&scalar(&output, BakedMap::Opacity, t.index))).count();
        eprintln!("opacity of a hole, {antialiasing:?}: worst error {worst:.2e}, {partial} partial texels");
        assert!(worst < 1e-6, "opacity {antialiasing:?} off by {worst}");
    }
}

// ---------------------------------------------------------------------------------------------
// Normal-map round trip: bake, then decode as the viewport does

/// The viewport's tangent frame at barycentrics `(b1, b2)` of `triangle`, as `shade_mesh` in
/// `crates/render/src/shaders/mesh.wgsl` builds it from the interpolated vertex attributes:
/// `n = normalize(nrm)`, `t = normalize(tan.xyz − n (n · tan.xyz))`, `b = cross(n, t) · tan.w`
/// (`tan.w` interpolated too).
fn viewport_frame(mesh: &Mesh, triangle: u32, [b1, b2]: [f32; 2]) -> (Vec3, Vec3, Vec3) {
    let [a, b, c] = mesh.tri(triangle as usize).unwrap();
    let weights = [1.0 - b1 - b2, b1, b2];
    let normal = (mesh.normals[a] * weights[0] + mesh.normals[b] * weights[1] + mesh.normals[c] * weights[2]).normalize();
    let tangent = mesh.tangents[a] * weights[0] + mesh.tangents[b] * weights[1] + mesh.tangents[c] * weights[2];
    let t = (tangent.truncate() - normal * normal.dot(tangent.truncate())).normalize();
    (t, normal.cross(t) * tangent.w, normal)
}

/// A world-space normal from a tangent-space one in a frame: `normalize(t·x + b·y + n·z)`
fn to_world((t, b, n): (Vec3, Vec3, Vec3), tangent_space: Vec3) -> Vec3 {
    (t * tangent_space.x + b * tangent_space.y + n * tangent_space.z).normalize()
}

/// The baked normal as the viewport reads it. `compose::display::tile_bytes` re-orients the
/// (flat) painted normal onto the baked one with `core::blend::rnm` and stores
/// `(v · 255 + 0.5) as u8`; the shader's `snorm8` reads that byte as `(byte − 128) / 127`.
fn viewport_tangent_normal(baked: Vec3) -> Vec3 {
    let shown = texturecraft_core::blend::rnm(baked.to_array(), [0.5, 0.5, 1.0]);
    let byte = |v: f32| f32::from((v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
    Vec3::from(shown.map(|v| ((byte(v) - 128.0) / 127.0).clamp(-1.0, 1.0))).normalize()
}

/// The high poly's shading normal where the bake's transfer ray from `surface` lands: from
/// `pos + N · frontal` back along `−N` for up to frontal + rear, skipping back faces (the
/// default). `N` is the interpolated normal, which is the averaged ray normal for these smooth,
/// welded low polys.
fn transfer_normal(high: &Mesh, bvh: &Bvh, surface: &Surface, frontal: f32, rear: f32) -> Option<Vec3> {
    let direction = -surface.nrm;
    let facing = |triangle: u32| {
        let [a, b, c] = high.tri(triangle as usize).unwrap();
        (high.positions[b] - high.positions[a]).cross(high.positions[c] - high.positions[a]).dot(direction) < 0.0
    };
    let hit = bvh.closest_filtered(surface.pos + surface.nrm * frontal, direction, frontal + rear, &facing)?;
    Some(surface_at(high, hit.tri, hit.u, hit.v)?.nrm)
}

/// Average the normals of vertices at the same position, so the high poly's shading normal is
/// continuous across its own UV seam (`compute_normals` keeps the seam's split vertices apart, and they sit a rounding error apart,
/// and a transfer ray landing on the seam edge could take either side's normal).
fn weld_normals(mesh: &mut Mesh) {
    let key = |p: &Vec3| p.to_array().map(|c| (c * 1e5).round() as i64);
    let mut sums = std::collections::HashMap::new();
    for (p, n) in mesh.positions.iter().zip(&mesh.normals) {
        *sums.entry(key(p)).or_insert(Vec3::ZERO) += *n;
    }
    for (p, n) in mesh.positions.iter().zip(&mut mesh.normals) {
        *n = sums[&key(p)].normalize();
    }
    mesh.compute_tangents();
}

/// p50, p99 and max of a set of angles
fn percentiles(mut angles: Vec<f32>) -> [f32; 3] {
    angles.sort_by(f32::total_cmp);
    let at = |q: f32| angles[((angles.len() - 1) as f32 * q) as usize];
    [at(0.5), at(0.99), at(1.0)]
}

/// A sphere with smooth bumps of up to about 15° over a 32 × 16 UV sphere, baked to a tangent
/// normal map at 256² and decoded per texel exactly as the viewport does. The decoded normal
/// must be the high-poly normal the transfer ray hit.
///
/// Float decoding isolates the conventions: the bake and the viewport build the same frame, so
/// the error is rounding. The 8-bit display path adds quantization and a bias: the display
/// stores `round(255 (½ + ½x))` = `127.5 (1 + x) ± ½` and the shader's `snorm8` reads it as
/// `(byte − 128) / 127` = `1.004 x − 0.004 ± 0.004`, so each component is off by at most
/// 0.0079 and the decoded direction by at most √3 · 0.0079 rad ≈ 0.78°.
///
/// Triangles whose vertices disagree on the bitangent sign are reported apart: the bake takes
/// the sign of their first vertex, the viewport interpolates it (see
/// `mixed_tangent_signs_decode_like_the_viewport`).
fn round_trip(tracer: &dyn Tracer, low: &Mesh, label: &str) {
    let mut high = primitives::sphere(1.0, 256, 128);
    for p in &mut high.positions {
        *p *= 1.0 + 0.03 * (9.0 * p.x).sin() * (7.0 * p.y).cos() * (5.0 * p.z + 1.0).sin();
    }
    high.compute_normals();
    weld_normals(&mut high);
    let (frontal, rear) = (0.06, 0.06);
    let mut settings = settings(256, BakeAntialiasing::None, &[BakedMap::Normal]);
    cage(&mut settings, frontal, rear);
    let output = bake_with(tracer, low, Some(&high), &settings);
    let bvh = Bvh::new(&high);
    let gbuffer = UvGBuffer::new(low, 0, 256, 256);
    let (mut float_errors, mut display_errors, mut mixed_errors, mut missed) = (Vec::new(), Vec::new(), Vec::new(), 0);
    for index in 0..256 * 256 {
        let Some(surface) = gbuffer.surface(low, index) else { continue };
        let Some(reference) = transfer_normal(&high, &bvh, &surface, frontal, rear) else {
            missed += 1;
            continue;
        };
        let frame = viewport_frame(low, gbuffer.tri[index], gbuffer.bary[index]);
        let baked = vector(&output, BakedMap::Normal, index);
        let error = degrees_between(to_world(frame, unpack(baked)), reference);
        if mixed_signs(low, gbuffer.tri[index]) {
            mixed_errors.push(error);
            continue;
        }
        float_errors.push(error);
        display_errors.push(degrees_between(to_world(frame, viewport_tangent_normal(baked)), reference));
    }
    let [float_p50, float_p99, float_max] = percentiles(float_errors.clone());
    let [p50, p99, max] = percentiles(display_errors);
    let tilt = percentiles(
        (0..256 * 256)
            .filter_map(|index| {
                let surface = gbuffer.surface(low, index)?;
                Some(degrees_between(transfer_normal(&high, &bvh, &surface, frontal, rear)?, surface.nrm))
            })
            .collect(),
    );
    let mixed_max = mixed_errors.iter().copied().fold(0.0, f32::max);
    eprintln!(
        "normal round trip ({label}, {} texels, {missed} missed, bump tilt p50 {:.1}° max {:.1}°): float p50 {float_p50:.4}° p99 {float_p99:.4}° max {float_max:.4}°; 8-bit p50 {p50:.3}° p99 {p99:.3}° max {max:.3}°; {} texels on mixed-sign triangles, float max {mixed_max:.3}°",
        float_errors.len(),
        tilt[0],
        tilt[2],
        mixed_errors.len(),
    );
    assert_eq!(missed, 0, "{label}: transfer rays that found no high poly");
    // Single-precision rounding of hit points, frames and the 0..1 packing: far below 0.1°
    assert!(float_p99 < 0.05 && float_max < 0.1, "{label}: float decoding p99 {float_p99}°, max {float_max}°");
    assert!(p50 < 0.45 && p99 < 0.75 && max < 0.8, "{label}: 8-bit decoding p50 {p50}°, p99 {p99}°, max {max}°");
}

/// Whether the vertices of `triangle` disagree on the bitangent sign
fn mixed_signs(mesh: &Mesh, triangle: u32) -> bool {
    let [a, b, c] = mesh.tri(triangle as usize).unwrap();
    let positive = [a, b, c].map(|v| mesh.tangents[v].w > 0.0);
    positive.iter().any(|p| *p != positive[0])
}

/// A triangle whose vertices disagree on the bitangent sign decodes differently in the bake and
/// the viewport: `surface_at` (and so the bake) takes the sign of the triangle's first vertex,
/// while `shade_mesh` interpolates `tan.w`, which shortens (and, past the midpoint, flips) the
/// bitangent. `Mesh::compute_tangents` produces such triangles where mirrored UVs meet at a
/// shared vertex: MikkTSpace gives that vertex a different sign in each face, and with one
/// tangent per vertex the last face written wins. The minimal case: two triangles sharing only
/// their apex, the second with its UVs mirrored in v, with tangents from `compute_tangents`,
/// baked from the tilted plane.
pub fn mixed_tangent_signs_decode_like_the_viewport(tracer: &dyn Tracer) {
    let n = Vec3::new(0.3, 1.0, -0.4).normalize();
    let positions = [Vec3::ZERO, Vec3::new(-1.0, 0.0, -1.0), Vec3::new(-1.0, 0.0, 1.0), Vec3::new(1.0, 0.0, -1.0), Vec3::new(1.0, 0.0, 1.0)];
    let uvs = positions.map(|p| Vec2::new(0.5 + 0.5 * p.x, 0.5 + 0.5 * p.z * if p.x > 0.0 { -1.0 } else { 1.0 }));
    // `from_parts` takes its tangents from `compute_tangents`
    let low = Mesh::from_parts(positions.to_vec(), Some(vec![Vec3::Y; 5]), uvs.to_vec(), vec![0, 1, 2, 0, 4, 3], "Bowtie").unwrap();
    let mut settings = settings(32, BakeAntialiasing::None, &[BakedMap::Normal]);
    cage(&mut settings, 1.0, 1.0);
    let output = bake_with(tracer, &low, Some(&tilted_plane(n)), &settings);
    let gbuffer = UvGBuffer::new(&low, 0, 32, 32);
    let covered: Vec<usize> = (0..32 * 32).filter(|index| gbuffer.covered(*index)).collect();
    assert!(covered.len() > 300, "{} texels covered", covered.len());
    let worst = covered
        .iter()
        .map(|&index| {
            degrees_between(to_world(viewport_frame(&low, gbuffer.tri[index], gbuffer.bary[index]), unpack(vector(&output, BakedMap::Normal, index))), n)
        })
        .fold(0.0, f32::max);
    eprintln!("mirrored UVs at a shared vertex: the viewport decodes the tilted plane up to {worst:.2}° off");
    assert!(worst < 0.1, "the viewport decodes the bake up to {worst}° off where mirrored UVs meet at a vertex");
}

/// The round trip on a UV sphere (MikkTSpace tangents, bitangent sign +1)
pub fn normal_map_round_trip(tracer: &dyn Tracer) {
    round_trip(tracer, &primitives::sphere(1.0, 32, 16), "UV sphere");
}

/// The round trip on the same sphere with its UVs mirrored in u (bitangent sign −1)
pub fn normal_map_round_trip_with_mirrored_uvs(tracer: &dyn Tracer) {
    let mut low = primitives::sphere(1.0, 32, 16);
    for uv in &mut low.uvs {
        uv.x = 1.0 - uv.x;
    }
    low.compute_tangents();
    assert!(low.tangents.iter().all(|t| t.w < 0.0), "mirrored UVs flip every bitangent sign");
    round_trip(tracer, &low, "mirrored UV sphere");
}

// ---------------------------------------------------------------------------------------------
// Padding and seams

/// Two islands with different constant world normals (a floor at u 0.1..0.4, a wall at u
/// 0.6..0.9): every gutter texel within the padding width takes the value of the nearer island
/// (Chebyshev distance in texels, as the padding's breadth-first search counts), texels beyond
/// it keep the neutral value, and covered texels are untouched.
pub fn padding_fills_each_gutter_from_the_nearest_island(tracer: &dyn Tracer) {
    let island = |u0: f32| move |a: f32, b: f32| Vec2::new(u0 + (a + 1.0) * 0.15, 0.1 + (b + 1.0) * 0.4);
    let mut low = floor([-1.0, 1.0], [-1.0, 1.0], 0.0, island(0.1));
    let wall_uv = island(0.6);
    let corners = [Vec3::new(1.0, 0.0, -1.0), Vec3::new(1.0, 2.0, -1.0), Vec3::new(1.0, 2.0, 1.0), Vec3::new(1.0, 0.0, 1.0)];
    let mut wall = face(corners, Vec3::X, corners.map(|c| wall_uv(c.y - 1.0, c.z)), "Floor");
    // One material, so both islands are baked
    wall.submeshes.clear();
    wall.materials.clear();
    let triangles = low.indices.len() as u32;
    low.append(&wall);
    low.submeshes[0].count = triangles + 6;

    let size = 64usize;
    let gbuffer = UvGBuffer::new(&low, 0, size as u32, size as u32);
    let owner = |index: usize| gbuffer.covered(index).then(|| usize::from(gbuffer.tri[index] >= 2));
    // Chebyshev distance from each texel to the nearest covered texel of each island
    let distance = |index: usize, island: usize| {
        let (x, y) = ((index % size) as i64, (index / size) as i64);
        (0..size * size)
            .filter(|other| owner(*other) == Some(island))
            .map(|other| ((other % size) as i64 - x).abs().max(((other / size) as i64 - y).abs()))
            .min()
            .unwrap()
    };
    let values = [pack(Vec3::Y), pack(Vec3::X)];
    for dilation in [3u32, 16] {
        let mut settings = settings(size as u32, BakeAntialiasing::None, &[BakedMap::WorldNormal]);
        settings.common.dilation = dilation;
        let output = bake_with(tracer, &low, None, &settings);
        let (mut filled, mut wrong) = (0, Vec::new());
        for index in 0..size * size {
            let value = vector(&output, BakedMap::WorldNormal, index);
            let expected = match owner(index) {
                Some(island) => Some(values[island]),
                None => {
                    let (near_floor, near_wall) = (distance(index, 0), distance(index, 1));
                    match near_floor.min(near_wall) > i64::from(dilation) {
                        true => Some(pack(Vec3::Z)),
                        false if near_floor == near_wall => None,
                        false => {
                            filled += 1;
                            Some(values[usize::from(near_wall < near_floor)])
                        }
                    }
                }
            };
            if expected.is_some_and(|expected| !value.abs_diff_eq(expected, 1e-6)) {
                wrong.push((index % size, index / size, value));
            }
        }
        eprintln!("padding {dilation}: {filled} gutter texels filled, {} wrong", wrong.len());
        assert!(filled > 0 && wrong.is_empty(), "padding {dilation}: {} texels hold the wrong value, e.g. {:?}", wrong.len(), wrong.first());
    }
}

/// A unit cylinder split into two halves with different UV layouts: θ ∈ 0..π along u, and
/// θ ∈ π..2π rotated 90° (θ along v). The halves meet at two seams with different tangent
/// frames on each side.
fn split_cylinder() -> Mesh {
    const SEGMENTS: usize = 32;
    const ROWS: usize = 8;
    let (mut positions, mut normals, mut uvs, mut indices) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for half in 0..2 {
        let base = positions.len() as u32;
        for i in 0..=SEGMENTS {
            let s = i as f32 / SEGMENTS as f32;
            let theta = PI * (half as f32 + s);
            for j in 0..=ROWS {
                let t = j as f32 / ROWS as f32;
                let normal = Vec3::new(theta.cos(), 0.0, theta.sin());
                positions.push(normal + Vec3::Y * (2.0 * t - 1.0));
                normals.push(normal);
                uvs.push(if half == 0 { Vec2::new(0.05 + 0.4 * s, 0.9 - 0.8 * t) } else { Vec2::new(0.55 + 0.4 * (1.0 - t), 0.1 + 0.8 * s) });
            }
        }
        for i in 0..SEGMENTS as u32 {
            for j in 0..ROWS as u32 {
                let at = |i: u32, j: u32| base + i * (ROWS as u32 + 1) + j;
                indices.extend([at(i, j), at(i, j + 1), at(i + 1, j + 1), at(i, j), at(i + 1, j + 1), at(i + 1, j)]);
            }
        }
    }
    let mut mesh = Mesh::from_parts(positions, Some(normals), uvs, indices, "Cylinder").unwrap();
    // Outward winding
    let [a, b, c] = mesh.tri(0).unwrap();
    if (mesh.positions[b] - mesh.positions[a]).cross(mesh.positions[c] - mesh.positions[a]).dot(mesh.normals[a]) < 0.0 {
        for triangle in mesh.indices.as_chunks_mut::<3>().0 {
            triangle.swap(1, 2);
        }
        mesh.compute_tangents();
    }
    mesh
}

/// A fine tube of radius `1 + 0.04 sin 4y` facing outwards, the seam scene's high poly
fn ringed_tube() -> Mesh {
    const SEGMENTS: u32 = 256;
    const ROWS: u32 = 256;
    let mut positions = Vec::new();
    for i in 0..=SEGMENTS {
        let theta = std::f32::consts::TAU * i as f32 / SEGMENTS as f32;
        for j in 0..=ROWS {
            let y = -1.2 + 2.4 * j as f32 / ROWS as f32;
            let r = 1.0 + 0.04 * (4.0 * y).sin();
            positions.push(Vec3::new(r * theta.cos(), y, r * theta.sin()));
        }
    }
    let at = |i: u32, j: u32| i * (ROWS + 1) + j;
    let mut indices = Vec::new();
    for i in 0..SEGMENTS {
        for j in 0..ROWS {
            // Outward: (p(i, j+1) − p(i, j)) × (p(i+1, j) − p(i, j)) = +y × +θ points out
            indices.extend([at(i, j), at(i, j + 1), at(i + 1, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1)]);
        }
    }
    let uvs = vec![Vec2::ZERO; positions.len()];
    let tube = Mesh::from_parts(positions, None, uvs, indices, "Tube").unwrap();
    assert!(tube.normals[at(0, ROWS / 2) as usize].x > 0.9, "the tube faces outwards");
    tube
}

/// The triangle of `mesh` among `triangles` whose UVs contain `uv`, with its barycentrics
fn locate(mesh: &Mesh, triangles: std::ops::Range<u32>, uv: Vec2) -> Option<(u32, [f32; 2])> {
    triangles.into_iter().find_map(|triangle| {
        let [a, b, c] = mesh.tri(triangle as usize)?;
        let (ua, ub, uc) = (mesh.uvs[a], mesh.uvs[b], mesh.uvs[c]);
        let area = (ub - ua).perp_dot(uc - ua);
        let (b1, b2) = ((uv - ua).perp_dot(uc - ua) / area, (ub - ua).perp_dot(uv - ua) / area);
        (b1 >= -1e-5 && b2 >= -1e-5 && b1 + b2 <= 1.0 + 1e-5).then_some((triangle, [b1, b2]))
    })
}

/// Across both seams of the split cylinder, the tangent-space normals baked on each side decode
/// to the same world normal at the same point, and so do the world-space normals.
///
/// Each side is sampled bilinearly at the seam point, so it reads texels up to 1.5 texels
/// inside its island (one half texel to the centre, one padded texel that copies a border texel
/// that may be a diagonal neighbour). The high poly is a cylinder with rings, radius
/// `1 + 0.04 sin 4y`, whose tilt from the low normal `atan(0.16 cos 4y)` varies only along y,
/// by at most 0.64 rad per unit, and the tangent-space value encodes only that tilt; 1.5 texels
/// along y are at most 0.03 units (the rotated half: 2 units over 0.4 · 256 texels), so the two
/// sides may differ by 2 · 0.64 · 0.03 rad ≈ 2.2°. The world-space normal also turns with θ: 1.5
/// texels are at most 1.5 π / (0.4 · 256) rad ≈ 2.6° on the narrower half and 1.3° on the other,
/// plus the same 2.2° along y.
pub fn tangent_normals_agree_across_a_uv_seam(tracer: &dyn Tracer) {
    let low = split_cylinder();
    let high = ringed_tube();
    let mut settings = settings(256, BakeAntialiasing::None, &[BakedMap::Normal, BakedMap::WorldNormal]);
    cage(&mut settings, 0.1, 0.1);
    settings.common.dilation = 4;
    let output = bake_with(tracer, &low, Some(&high), &settings);

    let half = low.triangle_count() as u32 / 2;
    let (mut tangent_worst, mut world_worst, mut points) = (0.0f32, 0.0f32, 0);
    // Seam θ = 0 (= 2π) and θ = π, at heights y
    for seam in [0.0f32, 1.0] {
        for step in -9..=9 {
            let y = step as f32 * 0.1;
            let t = (y + 1.0) * 0.5;
            let uv_first = Vec2::new(0.05 + 0.4 * seam, 0.9 - 0.8 * t);
            let uv_second = Vec2::new(0.55 + 0.4 * (1.0 - t), 0.1 + 0.8 * (1.0 - seam));
            let decoded = |uv: Vec2, triangles: std::ops::Range<u32>| {
                let (triangle, bary) = locate(&low, triangles, uv).unwrap();
                let frame = viewport_frame(&low, triangle, bary);
                (to_world(frame, unpack(bilinear(&output, BakedMap::Normal, uv))), unpack(bilinear(&output, BakedMap::WorldNormal, uv)))
            };
            let (first, first_world) = decoded(uv_first, 0..half);
            let (second, second_world) = decoded(uv_second, half..2 * half);
            tangent_worst = tangent_worst.max(degrees_between(first, second));
            world_worst = world_worst.max(degrees_between(first_world, second_world));
            points += 1;
        }
    }
    eprintln!("UV seam ({points} points): decoded tangent normals differ by at most {tangent_worst:.2}°, world normals by {world_worst:.2}°");
    assert!(tangent_worst < 2.5, "decoded tangent-space normals differ by {tangent_worst}° across the seam");
    assert!(world_worst < 5.0, "world-space normals differ by {world_worst}° across the seam");
}
