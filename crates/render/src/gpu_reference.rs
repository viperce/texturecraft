//! The bake crate's analytic reference scenes on the GPU tracer: the same scenes and tolerances
//! as `crates/bake/tests/reference.rs` on the CPU.
#![cfg(not(target_arch = "wasm32"))]

#[path = "../../bake/tests/reference_scenes/mod.rs"]
mod reference_scenes;

use crate::gpu_bake::GpuBaker;
use texturecraft_bake::{BakeRequest, Progress, bake_on};
use texturecraft_core::{BakeSettings, BakedMap};
use texturecraft_geom::{Bvh, primitives};

/// The GPU baker, or None (test skipped) when a small occlusion bake falls back to the CPU.
fn gpu() -> Option<GpuBaker> {
    let gpu = GpuBaker::new();
    let mesh = primitives::cube(2.0);
    let bvh = Bvh::new(&mesh);
    let mut settings = BakeSettings::default();
    settings.common.resolution = texturecraft_core::BakeResolution::R32;
    settings.maps.select([BakedMap::AmbientOcclusion]);
    let request = BakeRequest {
        low_base_color: &[],
        mesh: &mesh,
        bvh: &bvh,
        material: 0,
        settings: &settings,
        high: None,
        cage: None,
        skew: None,
        cage_paint: None,
        tiles: (1, 1),
    };
    let tracer = bake_on(&request, &Progress::new(), &gpu).ok()?.tracer;
    if tracer.starts_with("GPU") {
        Some(gpu)
    } else {
        eprintln!("skipping: {tracer}");
        None
    }
}

#[test]
fn gpu_bakes_the_reference_scenes() {
    let Some(gpu) = gpu() else { return };
    for (name, check) in reference_scenes::CHECKS {
        eprintln!("{name}");
        check(&gpu);
    }
}

#[test]
#[ignore = "fails until the per-corner tangent sign fix: compute_tangents keeps one tangent per vertex, so mirrored UVs meeting at a vertex mix bitangent signs in a triangle"]
fn gpu_mixed_tangent_signs_decode_like_the_viewport() {
    let Some(gpu) = gpu() else { return };
    reference_scenes::mixed_tangent_signs_decode_like_the_viewport(&gpu);
}
