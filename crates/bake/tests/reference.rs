//! The analytic reference scenes on the CPU tracer (`reference_scenes` has the derivations)

mod reference_scenes;

use texturecraft_bake::tracing::CpuTracer;

fn run(name: &str) {
    let (_, check) = reference_scenes::CHECKS.iter().find(|(check, _)| *check == name).unwrap();
    check(&CpuTracer);
}

#[test]
fn occlusion_beside_a_wall() {
    run("occlusion_beside_a_wall");
}

#[test]
fn occlusion_inside_an_open_box() {
    run("occlusion_inside_an_open_box");
}

#[test]
fn height_of_a_step() {
    run("height_of_a_step");
}

#[test]
fn height_of_a_spherical_cap() {
    run("height_of_a_spherical_cap");
}

#[test]
fn tangent_normal_of_a_tilted_plane() {
    run("tangent_normal_of_a_tilted_plane");
}

#[test]
fn world_normal_and_position() {
    run("world_normal_and_position");
}

#[test]
fn thickness_of_a_slab() {
    run("thickness_of_a_slab");
}

#[test]
fn opacity_of_a_hole() {
    run("opacity_of_a_hole");
}

#[test]
fn normal_map_round_trip() {
    run("normal_map_round_trip");
}

#[test]
fn normal_map_round_trip_with_mirrored_uvs() {
    run("normal_map_round_trip_with_mirrored_uvs");
}

#[test]
fn padding_fills_each_gutter_from_the_nearest_island() {
    run("padding_fills_each_gutter_from_the_nearest_island");
}

#[test]
fn tangent_normals_agree_across_a_uv_seam() {
    run("tangent_normals_agree_across_a_uv_seam");
}

#[test]
#[ignore = "fails until the per-corner tangent sign fix: compute_tangents keeps one tangent per vertex, so mirrored UVs meeting at a vertex mix bitangent signs in a triangle"]
fn mixed_tangent_signs_decode_like_the_viewport() {
    reference_scenes::mixed_tangent_signs_decode_like_the_viewport(&CpuTracer);
}
