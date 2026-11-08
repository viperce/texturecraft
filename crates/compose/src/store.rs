//! Pixel data owned by a document: painted layer channels, painted mask effects and baked mesh
//! maps. Everything is copy-on-write, so cloning the store for undo is cheap.

use std::collections::HashMap;

use texturecraft_core::{Channel, ChannelKind, LayerId};
use texturecraft_texel::{TileImage, from_u16, from_u16_signed};

/// Painted data of one paint layer channel: premultiplied value(s) plus coverage, u16 per
/// component (height stored signed).
#[derive(Clone, Debug, Default)]
pub struct PixelStore {
    /// (paint layer or content paint effect, channel) → texels.
    pub layers: HashMap<(LayerId, Channel), TileImage>,
    /// Paint effect of a mask → (premultiplied value, coverage).
    pub masks: HashMap<LayerId, TileImage>,
}

impl PixelStore {
    pub fn layer(&self, id: LayerId, c: Channel) -> Option<&TileImage> {
        self.layers.get(&(id, c))
    }

    /// The image for `(id, c)` at `size` (width, height), created empty if missing or of
    /// another size.
    pub fn layer_mut(&mut self, id: LayerId, c: Channel, size: (u32, u32)) -> &mut TileImage {
        sized(self.layers.entry((id, c)).or_insert_with(|| TileImage::new(size.0, size.1, c.components() + 1)), size)
    }

    pub fn mask_mut(&mut self, id: LayerId, size: (u32, u32)) -> &mut TileImage {
        sized(self.masks.entry(id).or_insert_with(|| TileImage::new(size.0, size.1, 2)), size)
    }

    /// Drop pixels of layers that no longer exist.
    pub fn retain(&mut self, alive: impl Fn(LayerId) -> bool) {
        self.layers.retain(|(id, _), _| alive(*id));
        self.masks.retain(|id, _| alive(*id));
    }

    pub fn bytes(&self) -> usize {
        self.layers.values().chain(self.masks.values()).map(TileImage::bytes).sum()
    }
}

/// `img`, replaced by an empty image of its components when it is not `size`.
fn sized(img: &mut TileImage, size: (u32, u32)) -> &mut TileImage {
    if (img.width(), img.height()) != size {
        *img = TileImage::new(size.0, size.1, img.comps());
    }
    img
}

/// Decode a stored component of channel kind `kind` to its float value.
#[inline]
pub fn decode(kind: ChannelKind, v: u16) -> f32 {
    if kind == ChannelKind::Signed { from_u16_signed(v) } else { from_u16(v) }
}

pub use crate::baked_maps::BakedMaps;

/// A decoded image resource sampled by fills (values in the file's encoding; see `srgb`).
#[derive(Clone, Debug, PartialEq)]
pub struct ImageRes {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<[f32; 4]>,
    /// 8/16-bit images are sRGB-encoded; color channels linearize them.
    pub srgb: bool,
}

/// Image resources by name.
pub type ImageStore = HashMap<String, std::sync::Arc<ImageRes>>;

impl ImageRes {
    fn texel(&self, x: i64, y: i64) -> [f32; 4] {
        let (w, h) = (self.width.max(1) as i64, self.height.max(1) as i64);
        let i = (y.rem_euclid(h) * w + x.rem_euclid(w)) as usize;
        self.rgba.get(i).copied().unwrap_or([0.0; 4])
    }

    /// Bilinear sample with wrap-around (texture coordinates, v down).
    pub fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        if !u.is_finite() || !v.is_finite() {
            return [0.0; 4];
        }
        let fx = u * self.width as f32 - 0.5;
        let fy = v * self.height as f32 - 0.5;
        if !fx.is_finite() || !fy.is_finite() {
            return [0.0; 4];
        }
        let (x0, y0) = (fx.floor() as i64, fy.floor() as i64);
        let (tx, ty) = (fx - fx.floor(), fy - fy.floor());
        let (a, b, c, d) = (
            self.texel(x0, y0),
            self.texel(x0.saturating_add(1), y0),
            self.texel(x0, y0.saturating_add(1)),
            self.texel(x0.saturating_add(1), y0.saturating_add(1)),
        );
        let mut out = [0.0; 4];
        for k in 0..4 {
            let top = a[k] + (b[k] - a[k]) * tx;
            let bot = c[k] + (d[k] - c[k]) * tx;
            out[k] = top + (bot - top) * ty;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2 x 2 black and white checker image for the sampling test.
    fn checker() -> ImageRes {
        ImageRes { width: 2, height: 2, rgba: vec![[0.0; 4], [1.0; 4], [1.0; 4], [0.0; 4]], srgb: false }
    }

    /// Sampling at huge finite coordinates stays finite and within 0..=1 instead of overflowing.
    #[test]
    fn sampling_at_huge_finite_coordinates_does_not_overflow() {
        let img = checker();
        for (u, v) in [(1.0e30, 0.5), (0.5, -1.0e30), (f32::MAX, f32::MAX), (1.0e6, -1.0e6)] {
            let s = img.sample(u, v);
            assert!(s.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c)), "({u}, {v}) -> {s:?}");
        }
    }
}
