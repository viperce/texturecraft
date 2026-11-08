//! Reading a MaterialX (`.mtlx`) material into TextureCraft's channels, written from the ASWF
//! MaterialX specification (1.39) and the OpenPBR Surface and glTF PBR node definitions. Pure
//! Rust: no MaterialX library is linked.
//!
//! [`read_materialx`] finds a `surfacematerial`, follows its `surfaceshader` to an
//! `open_pbr_surface`, `standard_surface` or `gltf_pbr` node, and turns each input TextureCraft
//! has a channel for into a literal value or an image file:
//!
//! - a literal `value` becomes a constant (`color3` values tagged `srgb_texture` are converted
//!   to linear; a colour given for a scalar channel is averaged);
//! - an input wired to a `tiledimage` or `image` node (directly or through a `nodegraph`
//!   output) becomes that node's `file` (with `fileprefix` applied); `normalmap`, `convert` and
//!   `constant` nodes in between are passed through;
//! - the material's `displacementshader` becomes the height channel;
//! - `uvtiling` of the first tiled image is returned as the UV tiling.
//!
//! Anything else (maths nodes, procedural patterns, other shaders) is skipped with a warning
//! rather than guessed at. Emission follows the specifications' defaults: OpenPBR emits nothing
//! unless `emission_luminance` is above 0, and `standard_surface` unless `emission` is.
//!
//! The reader takes untrusted bytes: the document size, nesting and element count are capped, a
//! wiring loop stops at a depth limit, and every failure is an error, never a panic.

use std::collections::HashMap;

use quick_xml::events::{BytesStart, Event};
use texturecraft_core::Channel;
use texturecraft_texel::srgb_to_linear;

use crate::{FormatError, Result, malformed};

const KIND: &str = "MaterialX document";
/// Largest document read, in bytes.
pub const MAX_BYTES: usize = 16 << 20;
const MAX_DEPTH: usize = 64;
const MAX_ELEMENTS: usize = 200_000;
/// Longest chain of nodes followed from one shader input.
const MAX_HOPS: usize = 16;

/// What one channel is set from.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// A constant: linear RGB for colours, `[v, v, v]` for scalars.
    Constant([f32; 3]),
    /// An image file, as written in the document (`fileprefix` applied), relative to it.
    Image(String),
}

/// A material read from a document.
#[derive(Clone, Debug, PartialEq)]
pub struct Material {
    pub name: String,
    /// The channels the material sets, in a fixed order.
    pub channels: Vec<(Channel, Value)>,
    /// The first image's `uvtiling` (1, 1 when none).
    pub uv_tiling: [f32; 2],
    /// What was skipped and why.
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------------------------
// XML.

#[derive(Default)]
struct El {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<El>,
}

impl El {
    fn attr(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }

    /// The child element named `name` (the `name` attribute: inputs and nodes are looked up by it).
    fn input(&self, name: &str) -> Option<&El> {
        self.children.iter().find(|c| c.attr("name") == Some(name))
    }
}

fn element(e: &BytesStart) -> El {
    let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    let attrs = e
        .attributes()
        .with_checks(false)
        .flatten()
        .map(|a| {
            (text(a.key.local_name().as_ref()), a.normalized_value(quick_xml::XmlVersion::Implicit1_0).map_or_else(|_| text(&a.value), |v| v.into_owned()))
        })
        .collect();
    El { name: text(e.local_name().as_ref()), attrs, children: Vec::new() }
}

fn parse_xml(bytes: &[u8]) -> Result<El> {
    let mut reader = quick_xml::Reader::from_reader(bytes);
    let mut stack = vec![El::default()];
    let mut count = 0usize;
    loop {
        let event = reader.read_event().map_err(|e| malformed(KIND, e))?;
        if matches!(event, Event::Start(_) | Event::Empty(_)) {
            count += 1;
            if count > MAX_ELEMENTS || stack.len() > MAX_DEPTH {
                return Err(FormatError::TooLarge(format!(
                    "the MaterialX document has more than {MAX_ELEMENTS} elements or is nested deeper than {MAX_DEPTH}"
                )));
            }
        }
        match event {
            Event::Start(e) => stack.push(element(&e)),
            Event::Empty(e) => {
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(element(&e));
                }
            }
            Event::End(_) => {
                let el = stack.pop().filter(|_| !stack.is_empty()).ok_or_else(|| malformed(KIND, "unbalanced end tag"))?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(el);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    match (stack.pop(), stack.is_empty()) {
        (Some(root), true) => Ok(root),
        _ => Err(malformed(KIND, "unclosed elements at the end of the document")),
    }
}

// ---------------------------------------------------------------------------------------------
// Shader input tables: (shader input, channel). `kind` says how to read the value.

#[derive(Clone, Copy, PartialEq)]
enum Shader {
    OpenPbr,
    Standard,
    Gltf,
}

const OPEN_PBR: &[(&str, Channel)] = &[
    ("base_color", Channel::BaseColor),
    ("base_metalness", Channel::Metallic),
    ("specular_roughness", Channel::Roughness),
    ("geometry_normal", Channel::Normal),
    ("geometry_opacity", Channel::Opacity),
    ("emission_color", Channel::Emissive),
    ("coat_weight", Channel::CoatWeight),
    ("coat_roughness", Channel::CoatRoughness),
    ("geometry_coat_normal", Channel::CoatNormal),
    ("fuzz_weight", Channel::SheenWeight),
    ("fuzz_color", Channel::SheenColor),
    ("fuzz_roughness", Channel::SheenRoughness),
    ("specular_roughness_anisotropy", Channel::AnisotropyLevel),
    ("transmission_weight", Channel::Transmission),
    ("subsurface_weight", Channel::ScatterWeight),
];

const STANDARD: &[(&str, Channel)] = &[
    ("base_color", Channel::BaseColor),
    ("metalness", Channel::Metallic),
    ("specular_roughness", Channel::Roughness),
    ("normal", Channel::Normal),
    ("opacity", Channel::Opacity),
    ("emission_color", Channel::Emissive),
    ("coat", Channel::CoatWeight),
    ("coat_roughness", Channel::CoatRoughness),
    ("coat_normal", Channel::CoatNormal),
    ("sheen", Channel::SheenWeight),
    ("sheen_color", Channel::SheenColor),
    ("sheen_roughness", Channel::SheenRoughness),
    ("specular_anisotropy", Channel::AnisotropyLevel),
    ("transmission", Channel::Transmission),
    ("subsurface", Channel::ScatterWeight),
];

const GLTF: &[(&str, Channel)] = &[
    ("base_color", Channel::BaseColor),
    ("metallic", Channel::Metallic),
    ("roughness", Channel::Roughness),
    ("normal", Channel::Normal),
    ("alpha", Channel::Opacity),
    ("emission", Channel::Emissive),
    ("occlusion", Channel::AmbientOcclusion),
    ("clearcoat", Channel::CoatWeight),
    ("clearcoat_roughness", Channel::CoatRoughness),
    ("clearcoat_normal", Channel::CoatNormal),
    ("sheen_color", Channel::SheenColor),
    ("sheen_roughness", Channel::SheenRoughness),
    ("transmission", Channel::Transmission),
];

// ---------------------------------------------------------------------------------------------
// Resolving what feeds an input.

struct Doc<'a> {
    root: &'a El,
    /// Every node by name: the document's own, and each nodegraph's (qualified `graph/node`).
    nodes: HashMap<String, &'a El>,
    /// Each nodegraph's outputs: `graph/output` to the node name inside (qualified).
    outputs: Vec<(String, String)>,
    tiling: Option<[f32; 2]>,
    warnings: Vec<String>,
}

enum Found {
    Constant(Vec<f32>),
    Image(String),
    None,
}

fn numbers(text: &str) -> Option<Vec<f32>> {
    let v: Option<Vec<f32>> = text.split(',').map(|p| p.trim().parse::<f32>().ok().filter(|f| f.is_finite())).collect();
    v.filter(|v| !v.is_empty() && v.len() <= 4)
}

impl<'a> Doc<'a> {
    fn new(root: &'a El) -> Doc<'a> {
        let mut nodes = HashMap::new();
        let mut outputs = Vec::new();
        for c in &root.children {
            match (c.name.as_str(), c.attr("name")) {
                ("nodegraph", Some(g)) => {
                    for n in &c.children {
                        let Some(name) = n.attr("name") else { continue };
                        if n.name == "output" {
                            if let Some(from) = n.attr("nodename") {
                                outputs.push((format!("{g}/{name}"), format!("{g}/{from}")));
                            }
                        } else {
                            nodes.insert(format!("{g}/{name}"), n);
                        }
                    }
                }
                (_, Some(name)) => {
                    nodes.insert(name.to_owned(), c);
                }
                _ => {}
            }
        }
        Doc { root, nodes, outputs, tiling: None, warnings: Vec::new() }
    }

    /// What `input` (an `<input>` element of a node) resolves to; `scope` is the nodegraph the
    /// node lives in, if any.
    fn resolve(&mut self, input: &El, scope: Option<&str>, hops: usize) -> Found {
        if hops > MAX_HOPS {
            self.warnings.push("a node chain is longer than 16 nodes (or loops); skipped".into());
            return Found::None;
        }
        if let Some(v) = input.attr("value") {
            return numbers(v).map_or(Found::None, |n| Found::Constant(self.tag(input, n)));
        }
        let target = match (input.attr("nodegraph"), input.attr("output"), input.attr("nodename")) {
            (Some(g), out, _) => {
                let prefix = format!("{g}/");
                let wanted = out.map(|o| format!("{prefix}{o}"));
                self.outputs.iter().find(|(k, _)| wanted.as_deref().map_or_else(|| k.starts_with(&prefix), |w| k == w)).map(|(_, v)| v.clone())
            }
            (None, _, Some(n)) => {
                Some(scope.map_or_else(|| n.to_owned(), |g| format!("{g}/{n}"))).filter(|q| self.nodes.contains_key(q)).or_else(|| Some(n.to_owned()))
            }
            _ => None,
        };
        let Some(node) = target.as_deref().and_then(|t| self.nodes.get(t).copied()) else { return Found::None };
        let scope = target.as_deref().and_then(|t| t.split_once('/')).map(|(g, _)| g.to_owned());
        let scope = scope.as_deref();
        match node.name.as_str() {
            "tiledimage" | "image" => {
                let Some(file) = node.input("file").and_then(|f| f.attr("value")) else {
                    self.warnings.push(format!("image node {:?} has no file", node.attr("name").unwrap_or("")));
                    return Found::None;
                };
                let prefix =
                    [node.input("file").and_then(|f| f.attr("fileprefix")), node.attr("fileprefix"), self.root.attr("fileprefix")].into_iter().flatten().next();
                if self.tiling.is_none()
                    && let Some(t) =
                        node.input("uvtiling").and_then(|t| t.attr("value")).and_then(numbers).filter(|t| t.len() == 2 && t.iter().all(|v| *v > 0.0))
                {
                    // The range a fill's UV scale accepts (`layer.fill.projection`).
                    self.tiling = Some([t[0].clamp(0.001, 1000.0), t[1].clamp(0.001, 1000.0)]);
                }
                Found::Image(format!("{}{file}", prefix.unwrap_or("")))
            }
            "normalmap" | "convert" => match node.input("in") {
                Some(i) => self.resolve(i, scope, hops + 1),
                None => Found::None,
            },
            "constant" => match node.input("value") {
                Some(v) => self.resolve(v, scope, hops + 1),
                None => Found::None,
            },
            other => {
                self.warnings.push(format!("a {other} node ({:?}) is not supported; its input was skipped", node.attr("name").unwrap_or("")));
                Found::None
            }
        }
    }

    /// Literal values tagged with an sRGB colour space are converted to linear.
    fn tag(&self, input: &El, mut n: Vec<f32>) -> Vec<f32> {
        let space = input.attr("colorspace").or_else(|| self.root.attr("colorspace")).unwrap_or("");
        if input.attr("colorspace").is_some() && (space.starts_with("srgb") || space.starts_with("g22") || space.starts_with("g18")) && n.len() >= 3 {
            n.iter_mut().take(3).for_each(|c| *c = srgb_to_linear(*c));
        }
        n
    }
}

fn to_constant(channel: Channel, n: &[f32]) -> [f32; 3] {
    match (channel.kind(), n) {
        (_, [a]) => [*a; 3],
        (texturecraft_core::ChannelKind::Color, [r, g, b, ..]) => [*r, *g, *b],
        (_, [r, g, b, ..]) => {
            let mean = (r + g + b) / 3.0;
            [mean; 3]
        }
        _ => Channel::default_value(channel),
    }
}

/// The literal number of `input`, if it is one.
fn literal(shader: &El, name: &str) -> Option<f32> {
    shader.input(name).and_then(|i| i.attr("value")).and_then(numbers).and_then(|n| n.first().copied())
}

/// Read the material called `material` (default: the first) from a `.mtlx` document.
pub fn read_materialx(bytes: &[u8], material: Option<&str>) -> Result<Material> {
    crate::guard(KIND, || read_inner(bytes, material))
}

fn read_inner(bytes: &[u8], material: Option<&str>) -> Result<Material> {
    if bytes.len() > MAX_BYTES {
        return Err(FormatError::TooLarge(format!("the MaterialX document is over {} MiB", MAX_BYTES >> 20)));
    }
    let top = parse_xml(bytes)?;
    let root = top.children.iter().find(|c| c.name == "materialx").ok_or_else(|| malformed(KIND, "no <materialx> element"))?;
    let mut doc = Doc::new(root);
    let materials: Vec<&El> = root.children.iter().filter(|c| c.name == "surfacematerial").collect();
    let mat = match material {
        Some(name) => materials
            .iter()
            .copied()
            .find(|m| m.attr("name") == Some(name))
            .ok_or_else(|| FormatError::Invalid(format!("no material named {name:?} (found: {})", names(&materials))))?,
        None => materials.first().copied().ok_or_else(|| FormatError::Unsupported("the document has no surfacematerial".into()))?,
    };
    let mut out = Material { name: mat.attr("name").unwrap_or("Material").to_owned(), channels: Vec::new(), uv_tiling: [1.0, 1.0], warnings: Vec::new() };
    let shader = mat
        .input("surfaceshader")
        .and_then(|i| i.attr("nodename"))
        .and_then(|n| doc.nodes.get(n).copied())
        .ok_or_else(|| malformed(KIND, "the material has no surface shader"))?;
    let (kind, table) = match shader.name.as_str() {
        "open_pbr_surface" => (Shader::OpenPbr, OPEN_PBR),
        "standard_surface" => (Shader::Standard, STANDARD),
        "gltf_pbr" => (Shader::Gltf, GLTF),
        other => return Err(FormatError::Unsupported(format!("the surface shader is a {other}; open_pbr_surface, standard_surface and gltf_pbr are read"))),
    };
    // Emission is off by default in OpenPBR and standard_surface.
    let emits = match kind {
        Shader::OpenPbr => {
            shader.input("emission_luminance").is_some_and(|i| i.attr("value").is_none() || literal(shader, "emission_luminance").is_some_and(|v| v > 0.0))
        }
        Shader::Standard => shader.input("emission").is_some_and(|i| i.attr("value").is_none() || literal(shader, "emission").is_some_and(|v| v > 0.0)),
        Shader::Gltf => true,
    };
    let weight = match kind {
        Shader::OpenPbr => literal(shader, "base_weight"),
        Shader::Standard => literal(shader, "base"),
        Shader::Gltf => None,
    };
    if weight.is_some_and(|w| (w - 1.0).abs() > 1e-6) {
        doc.warnings.push("the base weight isn't 1; it was ignored".into());
    }
    for &(input, channel) in table {
        let Some(el) = shader.input(input) else { continue };
        if channel == Channel::Emissive && !emits && kind != Shader::Gltf {
            doc.warnings.push(format!("{input} was skipped: the shader's emission is 0"));
            continue;
        }
        match doc.resolve(el, None, 0) {
            Found::Constant(n) => out.channels.push((channel, Value::Constant(to_constant(channel, &n)))),
            Found::Image(f) => out.channels.push((channel, Value::Image(f))),
            Found::None => {}
        }
    }
    // The displacement shader's `displacement` input is the height.
    if let Some(d) = mat.input("displacementshader").and_then(|i| i.attr("nodename")).and_then(|n| doc.nodes.get(n).copied())
        && let Some(i) = d.input("displacement")
    {
        match doc.resolve(i, None, 0) {
            Found::Constant(n) => out.channels.push((Channel::Height, Value::Constant(to_constant(Channel::Height, &n)))),
            Found::Image(f) => out.channels.push((Channel::Height, Value::Image(f))),
            Found::None => {}
        }
    }
    out.uv_tiling = doc.tiling.unwrap_or([1.0, 1.0]);
    out.warnings = doc.warnings;
    Ok(out)
}

fn names(materials: &[&El]) -> String {
    let n: Vec<&str> = materials.iter().filter_map(|m| m.attr("name")).collect();
    if n.is_empty() { "none".into() } else { n.join(", ") }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPENPBR: &str = r#"<?xml version="1.0"?>
<materialx version="1.39" colorspace="lin_rec709">
  <open_pbr_surface name="S" type="surfaceshader">
    <input name="base_color" type="color3" nodename="img_base" />
    <input name="base_metalness" type="float" value="1" />
    <input name="specular_roughness" type="float" nodename="img_rough" />
    <input name="geometry_normal" type="vector3" nodename="nm" />
    <input name="emission_color" type="color3" value="1, 0.5, 0" />
    <input name="emission_luminance" type="float" value="2" />
    <input name="coat_weight" type="float" value="0.25" />
  </open_pbr_surface>
  <tiledimage name="img_base" type="color3">
    <input name="file" type="filename" value="stone_base.png" colorspace="srgb_texture" />
    <input name="uvtiling" type="vector2" value="4, 2" />
  </tiledimage>
  <tiledimage name="img_rough" type="float"><input name="file" type="filename" value="stone_r.png" /></tiledimage>
  <tiledimage name="img_n" type="vector3"><input name="file" type="filename" value="stone_n.png" /></tiledimage>
  <normalmap name="nm" type="vector3"><input name="in" type="vector3" nodename="img_n" /></normalmap>
  <tiledimage name="img_h" type="float"><input name="file" type="filename" value="stone_h.png" /></tiledimage>
  <displacement name="D" type="displacementshader"><input name="displacement" type="float" nodename="img_h" /></displacement>
  <surfacematerial name="Stone" type="material">
    <input name="surfaceshader" type="surfaceshader" nodename="S" />
    <input name="displacementshader" type="displacementshader" nodename="D" />
  </surfacematerial>
</materialx>"#;

    fn get(m: &Material, c: Channel) -> Option<&Value> {
        m.channels.iter().find(|(ch, _)| *ch == c).map(|(_, v)| v)
    }

    #[test]
    fn open_pbr_inputs_become_channels() {
        let m = read_materialx(OPENPBR.as_bytes(), None).unwrap();
        assert_eq!(m.name, "Stone");
        assert_eq!(get(&m, Channel::BaseColor), Some(&Value::Image("stone_base.png".into())));
        assert_eq!(get(&m, Channel::Roughness), Some(&Value::Image("stone_r.png".into())));
        assert_eq!(get(&m, Channel::Normal), Some(&Value::Image("stone_n.png".into())), "through the normalmap node");
        assert_eq!(get(&m, Channel::Height), Some(&Value::Image("stone_h.png".into())), "from the displacement shader");
        assert_eq!(get(&m, Channel::Metallic), Some(&Value::Constant([1.0; 3])));
        assert_eq!(get(&m, Channel::CoatWeight), Some(&Value::Constant([0.25; 3])));
        assert_eq!(get(&m, Channel::Emissive), Some(&Value::Constant([1.0, 0.5, 0.0])));
        assert_eq!(m.uv_tiling, [4.0, 2.0]);
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
        let extreme = OPENPBR.replace(r#"value="4, 2""#, r#"value="1e9, 1e-9""#);
        assert_eq!(read_materialx(extreme.as_bytes(), None).unwrap().uv_tiling, [1000.0, 0.001], "clamped like a fill's UV scale");
    }

    #[test]
    fn nodegraph_input_without_output_uses_the_first_output() {
        let x = r#"<materialx version="1.38">
  <nodegraph name="NG">
    <tiledimage name="a" type="color3"><input name="file" type="filename" value="first.png" /></tiledimage>
    <tiledimage name="b" type="color3"><input name="file" type="filename" value="second.png" /></tiledimage>
    <output name="o1" type="color3" nodename="a" />
    <output name="o2" type="color3" nodename="b" />
  </nodegraph>
  <standard_surface name="S" type="surfaceshader"><input name="base_color" type="color3" nodegraph="NG" /></standard_surface>
  <surfacematerial name="M" type="material"><input name="surfaceshader" type="surfaceshader" nodename="S" /></surfacematerial>
</materialx>"#;
        for _ in 0..50 {
            let m = read_materialx(x.as_bytes(), None).unwrap();
            assert_eq!(get(&m, Channel::BaseColor), Some(&Value::Image("first.png".into())));
        }
    }

    #[test]
    fn emission_is_off_unless_the_shader_emits() {
        let x = OPENPBR.replace(r#"<input name="emission_luminance" type="float" value="2" />"#, "");
        let m = read_materialx(x.as_bytes(), None).unwrap();
        assert!(get(&m, Channel::Emissive).is_none());
        assert!(m.warnings.iter().any(|w| w.contains("emission")), "{:?}", m.warnings);
        let zero = OPENPBR.replace(r#"value="2" />"#, r#"value="0" />"#);
        assert!(get(&read_materialx(zero.as_bytes(), None).unwrap(), Channel::Emissive).is_none());
    }

    #[test]
    fn standard_surface_nodegraphs_prefixes_and_colour_spaces() {
        let x = r#"<materialx version="1.38" fileprefix="tex/">
  <nodegraph name="NG">
    <tiledimage name="t" type="color3"><input name="file" type="filename" value="a.png" /></tiledimage>
    <output name="out" type="color3" nodename="t" />
  </nodegraph>
  <standard_surface name="SS" type="surfaceshader">
    <input name="base_color" type="color3" nodegraph="NG" output="out" />
    <input name="specular_roughness" type="float" value="0.4" />
    <input name="opacity" type="color3" value="0.5, 0.5, 0.5" />
    <input name="sheen_color" type="color3" value="0.5, 0.5, 0.5" colorspace="srgb_texture" />
    <input name="base" type="float" value="0.5" />
  </standard_surface>
  <surfacematerial name="M1" type="material"><input name="surfaceshader" type="surfaceshader" nodename="SS" /></surfacematerial>
  <surfacematerial name="M2" type="material"><input name="surfaceshader" type="surfaceshader" nodename="SS" /></surfacematerial>
</materialx>"#;
        let m = read_materialx(x.as_bytes(), Some("M2")).unwrap();
        assert_eq!(m.name, "M2");
        assert_eq!(get(&m, Channel::BaseColor), Some(&Value::Image("tex/a.png".into())), "through the nodegraph, with the prefix");
        assert_eq!(get(&m, Channel::Roughness), Some(&Value::Constant([0.4; 3])));
        assert_eq!(get(&m, Channel::Opacity), Some(&Value::Constant([0.5; 3])), "a colour on a scalar channel is averaged");
        let Some(Value::Constant(c)) = get(&m, Channel::SheenColor) else { panic!("sheen colour") };
        assert!((c[0] - 0.214).abs() < 1e-3, "sRGB 0.5 is linear 0.214: {c:?}");
        assert!(m.warnings.iter().any(|w| w.contains("base weight")), "{:?}", m.warnings);
        assert!(read_materialx(x.as_bytes(), Some("nope")).is_err());
    }

    #[test]
    fn unsupported_nodes_and_shaders_are_reported() {
        let x = OPENPBR.replace(r#"<tiledimage name="img_rough""#, r#"<multiply name="img_rough" type="float"/><tiledimage name="unused""#);
        let m = read_materialx(x.as_bytes(), None).unwrap();
        assert!(get(&m, Channel::Roughness).is_none());
        assert!(m.warnings.iter().any(|w| w.contains("multiply")), "{:?}", m.warnings);
        let other = r#"<materialx><UsdPreviewSurface name="U" type="surfaceshader"/><surfacematerial name="M" type="material"><input name="surfaceshader" type="surfaceshader" nodename="U"/></surfacematerial></materialx>"#;
        assert!(matches!(read_materialx(other.as_bytes(), None), Err(FormatError::Unsupported(_))));
    }

    #[test]
    fn hostile_documents_are_errors() {
        for bad in ["", "not xml", "<materialx>", "<a><b></a>", "<materialx/>", "<materialx><surfacematerial name=\"M\"/></materialx>"] {
            assert!(read_materialx(bad.as_bytes(), None).is_err(), "{bad:?}");
        }
        // A wiring loop and a deep tree stop instead of running forever or overflowing.
        let loop_doc = r#"<materialx><open_pbr_surface name="S"><input name="base_color" nodename="c"/></open_pbr_surface>
            <convert name="c"><input name="in" nodename="c"/></convert>
            <surfacematerial name="M"><input name="surfaceshader" nodename="S"/></surfacematerial></materialx>"#;
        let m = read_materialx(loop_doc.as_bytes(), None).unwrap();
        assert!(m.channels.is_empty() && m.warnings.iter().any(|w| w.contains("loops")), "{:?}", m.warnings);
        let deep = format!("<materialx>{}{}</materialx>", "<a>".repeat(200), "</a>".repeat(200));
        assert!(matches!(read_materialx(deep.as_bytes(), None), Err(FormatError::TooLarge(_))));
        // Numbers: NaN, inf and over-long lists are not values.
        let nan = OPENPBR.replace(r#"value="0.25""#, r#"value="NaN""#);
        assert!(get(&read_materialx(nan.as_bytes(), None).unwrap(), Channel::CoatWeight).is_none());
    }

    #[test]
    fn xml_entities_in_file_names_are_decoded() {
        let x = OPENPBR.replace("stone_base.png", "a&amp;b &lt;1&gt;.png");
        let m = read_materialx(x.as_bytes(), None).unwrap();
        assert_eq!(get(&m, Channel::BaseColor), Some(&Value::Image("a&b <1>.png".into())));
    }

    #[test]
    fn mutated_documents_never_panic() {
        // Truncations and byte flips of a real document: errors or materials, never a panic.
        let bytes = OPENPBR.as_bytes();
        for cut in (0..bytes.len()).step_by(7) {
            let _ = read_materialx(&bytes[..cut], None);
        }
        for i in (0..bytes.len()).step_by(5) {
            let mut b = bytes.to_vec();
            b[i] ^= 0x5a;
            let _ = read_materialx(&b, None);
        }
    }
}
