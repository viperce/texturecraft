//! MaterialCraft portable materials (`.mcmat`, read only): a zip with `material.json` (the
//! manifest: maps by usage, exposed parameters, image inputs, presets), `maps/<usage>.png`
//! (8-bit sRGB for color usages, 16-bit linear otherwise; OpenGL normals) and the source
//! `.mcraft` package. TextureCraft keeps its own reader (the apps share the format, not code).
//!
//! Also the 16-bit PNG encoder that hands baked mesh maps to MaterialCraft as graph inputs.

use std::io::{Cursor, Read};

use serde::Deserialize;
use texturecraft_core::{LinkParam, LinkPreset};

use crate::{FormatError, Result};

/// The portable material extension.
pub const MCMAT_EXT: &str = "mcmat";
/// MaterialCraft's package extension.
pub const MCRAFT_EXT: &str = "mcraft";
const FORMAT: &str = "craft-material";
const VERSION: u32 = 1;
/// Largest entry read (maps are at most 8192² × 16 bits, compressed).
const MAX_ENTRY: u64 = 512 << 20;
/// Most bytes read from one file in all (the manifest, every map and the source package).
/// Without it a manifest that lists one compressible entry many times would be read, and kept,
/// once per listing.
const MAX_TOTAL: u64 = 2 << 30;
/// Largest `material.json` (a manifest is a few kilobytes; 1 MiB holds thousands of parameters).
const MAX_MANIFEST: u64 = 1 << 20;
/// Most maps a manifest may list (there are nine usages).
const MAX_MAPS: usize = 64;
const KIND: &str = "MaterialCraft material";

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub name: String,
    pub graph: String,
    pub size: [u32; 2],
    pub maps: Vec<MapEntry>,
    #[serde(default)]
    pub parameters: Vec<LinkParam>,
    #[serde(default)]
    pub inputs: Vec<ImageInput>,
    #[serde(default)]
    pub presets: Vec<LinkPreset>,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MapEntry {
    /// `baseColor`, `normal`, `roughness`, `metallic`, `height`, `ambientOcclusion`, `emissive`,
    /// `opacity` or `specular`.
    pub usage: String,
    pub file: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ImageInput {
    pub identifier: String,
}

/// A read `.mcmat`: the manifest, each map's PNG bytes by usage, the source package.
#[derive(Clone, Debug)]
pub struct PortableMaterial {
    pub manifest: Manifest,
    pub maps: Vec<(String, Vec<u8>)>,
    pub source: Vec<u8>,
}

fn malformed(msg: impl Into<String>) -> FormatError {
    FormatError::Malformed { kind: KIND, msg: msg.into() }
}

/// Read `.mcmat` bytes.
pub fn read_mcmat(bytes: &[u8]) -> Result<PortableMaterial> {
    read_within(bytes, MAX_ENTRY, MAX_TOTAL)
}

/// [`read_mcmat`] with the per-entry and whole-file byte limits as parameters (tests use small ones).
fn read_within(bytes: &[u8], max_entry: u64, max_total: u64) -> Result<PortableMaterial> {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| malformed(e.to_string()))?;
    let mut total = 0u64;
    let mut read = |name: &str, cap: u64| -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let f = z.by_name(name).map_err(|_| malformed(format!("{name} is missing")))?;
        // Never read past the entry's cap or what is left of the file's budget.
        let allowed = cap.min(max_entry).min(max_total.saturating_sub(total));
        f.take(allowed + 1).read_to_end(&mut out).map_err(|e| malformed(format!("{name}: {e}")))?;
        if out.len() as u64 > allowed {
            return Err(FormatError::TooLarge(name.to_string()));
        }
        total += out.len() as u64;
        Ok(out)
    };
    let manifest: Manifest = serde_json::from_slice(&read("material.json", MAX_MANIFEST)?).map_err(|e| malformed(format!("material.json: {e}")))?;
    if manifest.format != FORMAT {
        return Err(malformed(format!("format `{}`", manifest.format)));
    }
    if manifest.version > VERSION {
        return Err(FormatError::Unsupported(format!("{KIND} version {} (this version reads up to {VERSION})", manifest.version)));
    }
    if manifest.maps.len() > MAX_MAPS {
        return Err(FormatError::TooLarge(format!("{} maps listed (limit {MAX_MAPS})", manifest.maps.len())));
    }
    let maps = manifest.maps.iter().map(|m| Ok((m.usage.clone(), read(&m.file, MAX_ENTRY)?))).collect::<Result<Vec<_>>>()?;
    let source = read(&manifest.source, MAX_ENTRY)?;
    Ok(PortableMaterial { manifest, maps, source })
}

/// Whether `bytes` are a zip holding `entry` (`material.json`: a portable material,
/// `package.json`: a MaterialCraft package).
fn zip_has(bytes: &[u8], entry: &str) -> bool {
    zip::ZipArchive::new(Cursor::new(bytes)).is_ok_and(|mut z| z.by_name(entry).is_ok())
}

pub fn is_mcmat(bytes: &[u8]) -> bool {
    zip_has(bytes, "material.json")
}

pub fn is_mcraft(bytes: &[u8]) -> bool {
    zip_has(bytes, "package.json")
}

/// A baked mesh map (`components` floats per texel, 0..1, rows top first) as a 16-bit PNG
/// (grayscale for one component, RGB for three).
pub fn mesh_map_png(width: u32, components: usize, data: &[f32]) -> Result<Vec<u8>> {
    let per_row = (width as usize).saturating_mul(components);
    let height = data.len().checked_div(per_row).unwrap_or(0) as u32;
    if height == 0 || !matches!(components, 1 | 3) {
        return Err(FormatError::Invalid(format!("mesh map of {} values, {width} wide, {components} components", data.len())));
    }
    let px: Vec<u16> = data.iter().take(per_row * height as usize).map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16).collect();
    let img = if components == 1 {
        image::ImageBuffer::<image::Luma<u16>, _>::from_raw(width, height, px).map(image::DynamicImage::ImageLuma16)
    } else {
        image::ImageBuffer::<image::Rgb<u16>, _>::from_raw(width, height, px).map(image::DynamicImage::ImageRgb16)
    }
    .ok_or_else(|| FormatError::Invalid("mesh map size".into()))?;
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).map_err(|e| FormatError::Invalid(e.to_string()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A `.mcmat` as MaterialCraft writes it (see its `formats/src/material.rs`).
    pub(crate) fn sample(version: u32) -> Vec<u8> {
        let manifest = serde_json::json!({
            "format": "craft-material", "version": version, "name": "Brick wall", "graph": "Brick_wall", "size": [4, 4],
            "maps": [{"usage": "baseColor", "file": "maps/baseColor.png", "colorSpace": "srgb"}],
            "parameters": [{"id": "wear", "label": "Wear", "default": {"t": "float", "v": 0.5}, "min": 0.0, "max": 1.0, "value": {"t": "float", "v": 0.25}}],
            "inputs": [{"identifier": "curvature", "kind": "gray"}],
            "presets": [{"name": "Worn", "values": {"wear": {"t": "float", "v": 1.0}}}],
            "source": "source.mcraft", "futureField": true
        });
        let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("material.json", o).unwrap();
        z.write_all(manifest.to_string().as_bytes()).unwrap();
        z.start_file("maps/baseColor.png", o).unwrap();
        z.write_all(&mesh_map_png(4, 3, &[0.5; 48]).unwrap()).unwrap();
        z.start_file("source.mcraft", o).unwrap();
        z.write_all(b"pkg").unwrap();
        z.finish().unwrap().into_inner()
    }

    #[test]
    fn reads_manifest_maps_and_source() {
        let bytes = sample(1);
        assert!(is_mcmat(&bytes) && !is_mcraft(&bytes));
        let m = read_mcmat(&bytes).unwrap();
        assert_eq!(m.manifest.graph, "Brick_wall");
        assert_eq!(m.manifest.parameters[0].id, "wear");
        assert_eq!(m.manifest.parameters[0].value, serde_json::json!({"t": "float", "v": 0.25}));
        assert_eq!(m.manifest.presets[0].name, "Worn");
        assert_eq!(m.manifest.inputs[0].identifier, "curvature");
        assert_eq!(m.maps[0].0, "baseColor");
        assert_eq!(crate::decode_image(&m.maps[0].1).unwrap().width, 4);
        assert_eq!(m.source, b"pkg");
        assert!(matches!(read_mcmat(&sample(2)), Err(FormatError::Unsupported(_))));
        assert!(read_mcmat(b"junk").is_err());
    }

    /// A `.mcmat` whose manifest lists `maps` (file names) and whose zip holds those files.
    fn with_maps(maps: &[&str], files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let list: Vec<_> = maps.iter().map(|f| serde_json::json!({"usage": "baseColor", "file": f})).collect();
        let manifest =
            serde_json::json!({"format": "craft-material", "version": 1, "name": "M", "graph": "M", "size": [4, 4], "maps": list, "source": "source.mcraft"});
        let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("material.json", o).unwrap();
        z.write_all(manifest.to_string().as_bytes()).unwrap();
        z.start_file("source.mcraft", o).unwrap();
        z.write_all(b"pkg").unwrap();
        for (name, bytes) in files {
            z.start_file(*name, o).unwrap();
            z.write_all(bytes).unwrap();
        }
        z.finish().unwrap().into_inner()
    }

    /// A manifest over the size limit is refused as too large, without being read.
    #[test]
    fn an_oversized_manifest_is_refused() {
        // A manifest is a few kilobytes; one over MAX_MANIFEST (here a 2 MiB name) is not read.
        let manifest = serde_json::json!({"format": "craft-material", "version": 1, "name": "x".repeat(2 << 20), "graph": "M", "size": [4, 4], "maps": [], "source": "source.mcraft"});
        let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("material.json", o).unwrap();
        z.write_all(manifest.to_string().as_bytes()).unwrap();
        z.start_file("source.mcraft", o).unwrap();
        z.write_all(b"pkg").unwrap();
        let bytes = z.finish().unwrap().into_inner();
        assert!(matches!(read_mcmat(&bytes), Err(FormatError::TooLarge(_))));
    }

    /// More maps than the limit are refused, while a normal count still reads.
    #[test]
    fn too_many_maps_are_refused() {
        let names: Vec<String> = (0..=MAX_MAPS).map(|_| "m.png".to_string()).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let bytes = with_maps(&refs, &[("m.png", vec![1; 8])]);
        assert!(matches!(read_mcmat(&bytes), Err(FormatError::TooLarge(_))));
        // A normal count still reads.
        let bytes = with_maps(&["m.png"; 3], &[("m.png", vec![1; 8])]);
        assert_eq!(read_mcmat(&bytes).unwrap().maps.len(), 3);
    }

    /// An entry listed over and over stops at the total budget, and an entry over the per-entry cap is refused.
    #[test]
    fn one_entry_listed_again_and_again_is_bounded_by_the_total() {
        // With small limits: 100 KiB entries, 250 KiB for the whole file. Zeros compress to nearly
        // nothing, so the zip stays tiny however often the manifest lists the entry.
        let each = vec![0u8; 100 << 10];
        let (entry, total) = (110 << 10, 250 << 10);
        // One listing, and two, fit.
        assert!(read_within(&with_maps(&["e.bin"], &[("e.bin", each.clone())]), entry, total).is_ok());
        assert!(read_within(&with_maps(&["e.bin"; 2], &[("e.bin", each.clone())]), entry, total).is_ok());
        // Listed until the budget is spent, it stops there instead of reading it all.
        let bytes = with_maps(&["e.bin"; 40], &[("e.bin", each.clone())]);
        assert!(bytes.len() < 10 << 10, "the zip stays small: {}", bytes.len());
        assert!(matches!(read_within(&bytes, entry, total), Err(FormatError::TooLarge(_))));
        // An entry over the per-entry cap is refused on its own.
        let big = vec![0u8; 120 << 10];
        assert!(matches!(read_within(&with_maps(&["big.bin"], &[("big.bin", big)]), entry, total), Err(FormatError::TooLarge(_))));
    }

    #[test]
    fn mesh_maps_encode_as_16_bit() {
        let png = mesh_map_png(2, 1, &[0.0, 0.25, 0.5, 1.0]).unwrap();
        let d = crate::decode_image(&png).unwrap();
        assert_eq!((d.width, d.height), (2, 2));
        assert!((d.rgba[1][0] - 0.25).abs() < 1e-4);
        assert!(mesh_map_png(0, 1, &[]).is_err());
        assert!(mesh_map_png(2, 2, &[0.0; 8]).is_err());
    }
}
