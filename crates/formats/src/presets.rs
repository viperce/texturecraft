//! Built-in export presets and user preset JSON. Preset names are ours; their contents follow
//! each engine's public material conventions.

use texturecraft_core::{BakedMap, Channel, ChannelKind};

use crate::export::{Dest, ExportPreset, FileFormat, MapSpec, SourceRef, VirtualMap};
use crate::{FormatError, Result};

/// Largest preset JSON accepted.
const MAX_PRESET_JSON: usize = 1 << 20;

pub(crate) fn ch(channel: Channel, component: u8) -> SourceRef {
    SourceRef::Channel { channel, component }
}

pub(crate) fn virt(map: VirtualMap, component: u8) -> SourceRef {
    SourceRef::Virtual { map, component }
}

/// `$mesh_$textureSet_<suffix>` PNG.
fn map(suffix: &str, bit_depth: u8, channels: Vec<(Dest, SourceRef)>) -> MapSpec {
    MapSpec { file: format!("$mesh_$textureSet_{suffix}"), format: FileFormat::Png, bit_depth, channels }
}

pub(crate) fn rgb(f: impl Fn(u8) -> SourceRef) -> Vec<(Dest, SourceRef)> {
    vec![(Dest::R, f(0)), (Dest::G, f(1)), (Dest::B, f(2))]
}

pub(crate) fn gray(s: SourceRef) -> Vec<(Dest, SourceRef)> {
    vec![(Dest::L, s)]
}

/// Base color RGB with opacity in alpha (alpha is dropped when the set has no opacity).
fn base_with_alpha() -> Vec<(Dest, SourceRef)> {
    let mut c = rgb(|k| ch(Channel::BaseColor, k));
    c.push((Dest::A, ch(Channel::Opacity, 0)));
    c
}

fn normal(map_kind: VirtualMap) -> Vec<(Dest, SourceRef)> {
    rgb(|k| virt(map_kind, k))
}

/// One file per channel: RGB for colors and normals, grayscale (16-bit for height) otherwise.
fn channel_map(c: Channel) -> MapSpec {
    let (channels, depth) = match c.kind() {
        ChannelKind::Color | ChannelKind::NormalMap => (rgb(|k| ch(c, k)), 8),
        ChannelKind::Signed => (gray(ch(c, 0)), 16),
        ChannelKind::Scalar => (gray(ch(c, 0)), 8),
    };
    map("$channel", depth, channels)
}

fn orm() -> Vec<(Dest, SourceRef)> {
    vec![(Dest::R, virt(VirtualMap::AoMixed, 0)), (Dest::G, ch(Channel::Roughness, 0)), (Dest::B, ch(Channel::Metallic, 0))]
}

/// File stems of the glTF metallic-roughness maps (also the GLB export's texture slots).
pub(crate) const GLTF_BASE_COLOR: &str = "BaseColor";
pub(crate) const GLTF_ORM: &str = "OcclusionRoughnessMetallic";
pub(crate) const GLTF_NORMAL: &str = "Normal";
pub(crate) const GLTF_EMISSIVE: &str = "Emissive";
/// KHR_materials_clearcoat: clear coat weight (R) and roughness (G).
pub(crate) const GLTF_CLEARCOAT: &str = "Clearcoat";
/// KHR_materials_sheen: sheen color × weight (RGB) and roughness (A).
pub(crate) const GLTF_SHEEN: &str = "Sheen";

/// glTF metallic-roughness maps (8-bit PNG), plus the clear coat and sheen extension maps where
/// the texture set has those channels; `file` turns a stem above into a file template.
pub(crate) fn gltf_pbr_maps(file: impl Fn(&str) -> String) -> Vec<MapSpec> {
    let spec = |stem: &str, channels| MapSpec { file: file(stem), format: FileFormat::Png, bit_depth: 8, channels };
    let mut sheen = rgb(|k| virt(VirtualMap::SheenTint, k));
    sheen.push((Dest::A, ch(Channel::SheenRoughness, 0)));
    vec![
        spec(GLTF_BASE_COLOR, base_with_alpha()),
        spec(GLTF_ORM, orm()),
        spec(GLTF_NORMAL, normal(VirtualMap::NormalOpenGl)),
        spec(GLTF_EMISSIVE, rgb(|k| ch(Channel::Emissive, k))),
        spec(GLTF_CLEARCOAT, vec![(Dest::R, ch(Channel::CoatWeight, 0)), (Dest::G, ch(Channel::CoatRoughness, 0))]),
        spec(GLTF_SHEEN, sheen),
    ]
}

/// Every built-in preset, in menu order.
pub fn builtin_presets() -> Vec<ExportPreset> {
    let preset = |name: &str, maps: Vec<MapSpec>| ExportPreset { name: name.into(), maps };
    vec![
        preset("Document channels + Normal + AO", {
            let mut maps: Vec<MapSpec> =
                Channel::ALL.into_iter().filter(|c| !matches!(c, Channel::Normal | Channel::AmbientOcclusion)).map(channel_map).collect();
            maps.push(map("normal", 8, rgb(|k| ch(Channel::Normal, k))));
            maps.push(map("ambientOcclusion", 8, gray(virt(VirtualMap::AoMixed, 0))));
            maps
        }),
        preset("glTF PBR Metal Roughness", gltf_pbr_maps(|s| format!("$mesh_$textureSet_{s}"))),
        preset(
            "Unreal Engine (Packed)",
            vec![map("BaseColor", 8, base_with_alpha()), map("OcclusionRoughnessMetallic", 8, orm()), map("Normal", 8, normal(VirtualMap::NormalDirectX))],
        ),
        preset(
            "Unity HDRP (Mask Map)",
            vec![
                map("BaseMap", 8, base_with_alpha()),
                map(
                    "MaskMap",
                    8,
                    vec![
                        (Dest::R, ch(Channel::Metallic, 0)),
                        (Dest::G, virt(VirtualMap::AoMixed, 0)),
                        // Detail mask: none.
                        (Dest::B, SourceRef::Constant(0.0)),
                        (Dest::A, virt(VirtualMap::Glossiness, 0)),
                    ],
                ),
                map("Normal", 8, normal(VirtualMap::NormalOpenGl)),
            ],
        ),
        preset(
            "Unity Standard (Metallic Smoothness)",
            vec![
                map("Albedo", 8, base_with_alpha()),
                map(
                    "MetallicSmoothness",
                    8,
                    vec![
                        (Dest::R, ch(Channel::Metallic, 0)),
                        (Dest::G, SourceRef::Constant(0.0)),
                        (Dest::B, SourceRef::Constant(0.0)),
                        (Dest::A, virt(VirtualMap::Glossiness, 0)),
                    ],
                ),
                map("Normal", 8, normal(VirtualMap::NormalOpenGl)),
                map("Occlusion", 8, gray(virt(VirtualMap::AoMixed, 0))),
            ],
        ),
        preset(
            "Blender (Principled BSDF)",
            vec![
                map("BaseColor", 8, base_with_alpha()),
                map("Metallic", 8, gray(ch(Channel::Metallic, 0))),
                map("Roughness", 8, gray(ch(Channel::Roughness, 0))),
                map("Normal", 8, normal(VirtualMap::NormalOpenGl)),
                map("Height", 16, gray(ch(Channel::Height, 0))),
                map("Emission", 8, rgb(|k| ch(Channel::Emissive, k))),
                map("Specular", 8, gray(ch(Channel::SpecularLevel, 0))),
            ],
        ),
        // Roblox SurfaceAppearance: Color (opacity in alpha), Normal, Metalness, Roughness: suffixes
        // Roblox's importer recognises, so it assigns each map to its slot on reimport.
        // See `docs/export-roblox-sketchfab.md` for sources and the items still to verify.
        preset(
            "Roblox (SurfaceAppearance)",
            vec![
                map("Color", 8, base_with_alpha()),
                map("Normal", 8, normal(VirtualMap::NormalOpenGl)),
                map("Metalness", 8, gray(ch(Channel::Metallic, 0))),
                map("Roughness", 8, gray(ch(Channel::Roughness, 0))),
            ],
        ),
        // Sketchfab PBR metal-roughness: separate maps whose suffixes Sketchfab's importer
        // recognises (base color, normal, metallic, roughness, AO, emissive).
        preset(
            "Sketchfab (PBR Metal Roughness)",
            vec![
                map("BaseColor", 8, base_with_alpha()),
                map("Normal", 8, normal(VirtualMap::NormalOpenGl)),
                map("Metallic", 8, gray(ch(Channel::Metallic, 0))),
                map("Roughness", 8, gray(ch(Channel::Roughness, 0))),
                map("AmbientOcclusion", 8, gray(virt(VirtualMap::AoMixed, 0))),
                map("Emissive", 8, rgb(|k| ch(Channel::Emissive, k))),
            ],
        ),
        preset(
            "Godot 4 (ORM)",
            vec![
                map("albedo", 8, base_with_alpha()),
                map("orm", 8, orm()),
                map("normal", 8, normal(VirtualMap::NormalOpenGl)),
                map("emission", 8, rgb(|k| ch(Channel::Emissive, k))),
            ],
        ),
        // OpenPBR Surface inputs, one map each (maps of channels a set lacks are skipped).
        preset("OpenPBR (separate maps)", {
            let gray8 = |name: &str, c: Channel| map(name, 8, gray(ch(c, 0)));
            let rgb8 = |name: &str, c: Channel| map(name, 8, rgb(|k| ch(c, k)));
            vec![
                rgb8("base_color", Channel::BaseColor),
                gray8("base_metalness", Channel::Metallic),
                gray8("specular_roughness", Channel::Roughness),
                gray8("specular_roughness_anisotropy", Channel::AnisotropyLevel),
                gray8("coat_weight", Channel::CoatWeight),
                gray8("coat_roughness", Channel::CoatRoughness),
                gray8("fuzz_weight", Channel::SheenWeight),
                rgb8("fuzz_color", Channel::SheenColor),
                gray8("fuzz_roughness", Channel::SheenRoughness),
                gray8("transmission_weight", Channel::Transmission),
                gray8("subsurface_weight", Channel::ScatterWeight),
                rgb8("emission_color", Channel::Emissive),
                gray8("geometry_opacity", Channel::Opacity),
                map("geometry_normal", 8, normal(VirtualMap::NormalOpenGl)),
                map("height", 16, gray(ch(Channel::Height, 0))),
            ]
        }),
        preset("Separate maps", Channel::ALL.into_iter().map(channel_map).collect()),
        preset(
            "Mesh maps",
            BakedMap::ALL
                .into_iter()
                .map(|m| {
                    let src = |component| SourceRef::Baked { map: m, component };
                    map("$channel", 16, if m.components() == 3 { rgb(src) } else { gray(src(0)) })
                })
                .collect(),
        ),
    ]
}

/// Largest texture edge (pixels) a built-in preset is meant for, if its target caps it.
/// Roblox: 1024 is the figure community reports cite; the official limit is not confirmed
/// (see `docs/export-roblox-sketchfab.md`). `ExportOptions::size` is per UV tile, so for a
/// UV-tiled set divide this by the larger of the tile grid's columns and rows before passing it.
pub fn preset_max_size(name: &str) -> Option<u32> {
    name.trim().eq_ignore_ascii_case("Roblox (SurfaceAppearance)").then_some(1024)
}

/// A built-in preset by name (case-insensitive).
pub fn builtin_preset(name: &str) -> Option<ExportPreset> {
    builtin_presets().into_iter().find(|p| p.name.eq_ignore_ascii_case(name.trim()))
}

/// Parse and validate a user preset.
pub fn preset_from_json(json: &str) -> Result<ExportPreset> {
    if json.len() > MAX_PRESET_JSON {
        return Err(FormatError::TooLarge("export preset JSON over 1 MiB".into()));
    }
    let p: ExportPreset = serde_json::from_str(json).map_err(|e| crate::malformed("export preset", e))?;
    p.validate()?;
    Ok(p)
}

pub fn preset_to_json(preset: &ExportPreset) -> Result<String> {
    serde_json::to_string_pretty(preset).map_err(|e| FormatError::Invalid(e.to_string()))
}

/// Read a user preset file (size-capped, validated).
#[cfg(not(target_arch = "wasm32"))]
pub fn load_preset(path: &std::path::Path) -> Result<ExportPreset> {
    let bytes = crate::read_capped(path, MAX_PRESET_JSON as u64)?;
    preset_from_json(&String::from_utf8_lossy(&bytes))
}
