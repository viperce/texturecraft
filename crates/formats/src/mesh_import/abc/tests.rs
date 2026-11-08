//! Alembic Ogawa files built by a small writer (the layout the reader expects), then imported.

use texturecraft_geom::glam::Vec3;

use super::*;
use crate::mesh_import::finish;

/// Builds an Ogawa byte stream: data blocks and groups appended, the root set last.
struct Writer {
    b: Vec<u8>,
}

impl Writer {
    fn new() -> Writer {
        Writer { b: vec![0; 16] }
    }

    fn data(&mut self, bytes: &[u8]) -> u64 {
        let at = self.b.len() as u64;
        self.b.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        self.b.extend_from_slice(bytes);
        at | DATA_BIT
    }

    fn group(&mut self, children: &[u64]) -> u64 {
        let at = self.b.len() as u64;
        self.b.extend_from_slice(&(children.len() as u64).to_le_bytes());
        for c in children {
            self.b.extend_from_slice(&c.to_le_bytes());
        }
        at
    }

    fn finish(mut self, root: u64) -> Vec<u8> {
        self.b[..5].copy_from_slice(OGAWA_MAGIC);
        self.b[5] = 0xff;
        self.b[6..8].copy_from_slice(&[0, 1]);
        self.b[8..16].copy_from_slice(&root.to_le_bytes());
        self.b
    }
}

/// A property for [`Abc::compound`]: name, metadata, kind and group.
struct P {
    name: &'static str,
    meta: String,
    info: u32,
    group: u64,
    samples: u32,
    /// First and last changed sample index ((0, 0): constant).
    changed: (u32, u32),
}

/// Which samples an animated property stores (see `sample_slot`).
#[derive(Clone, Copy)]
enum Layout {
    /// Every index up to the last change.
    PerSample,
    /// Sample 0, then the indices from the first change to the last.
    Compact,
}

fn read(bytes: &[u8]) -> Result<RawMesh> {
    super::read(bytes, 0)
}

/// Alembic structure on top of [`Writer`].
struct Abc {
    w: Writer,
}

const F32: u32 = 10;
const I32: u32 = 6;
const U32: u32 = 5;
const U8: u32 = 1;
const F64: u32 = 11;

impl Abc {
    fn sample(&mut self, raw: &[u8]) -> u64 {
        let mut v = vec![7u8; KEY_BYTES];
        v.extend_from_slice(raw);
        self.w.data(&v)
    }

    fn array(&mut self, name: &'static str, meta: &str, pod: u32, extent: u32, raw: &[u8]) -> P {
        let (d, dims) = (self.sample(raw), self.w.data(&[]));
        let group = self.w.group(&[d, dims]);
        // Array, size hint 2 (u32 fields), inline metadata.
        P { name, meta: meta.into(), info: 2 | (2 << 2) | (pod << 4) | (extent << 12), group, samples: 1, changed: (0, 0) }
    }

    fn scalar(&mut self, name: &'static str, pod: u32, extent: u32, raw: &[u8]) -> P {
        let d = self.sample(raw);
        let group = self.w.group(&[d]);
        P { name, meta: String::new(), info: 1 | (2 << 2) | (pod << 4) | (extent << 12), group, samples: 1, changed: (0, 0) }
    }

    /// The group of an animated property with one sample per entry of `frames`, and the first and
    /// last indices that differ from their predecessor; `stride` children per sample.
    fn sample_group(&mut self, frames: &[Vec<u8>], layout: Layout, stride: usize) -> (u64, (u32, u32)) {
        let first = (1..frames.len()).find(|i| frames[*i] != frames[0]).unwrap_or(0);
        let last = (1..frames.len()).rev().find(|i| frames[*i] != frames[i - 1]).unwrap_or(0);
        let stored: Vec<usize> = match (first, layout) {
            (0, _) => vec![0],
            (_, Layout::PerSample) => (0..=last).collect(),
            (_, Layout::Compact) => std::iter::once(0).chain(first..=last).collect(),
        };
        let mut children = Vec::new();
        for i in stored {
            children.push(self.sample(&frames[i]));
            if stride == 2 {
                children.push(self.w.data(&[]));
            }
        }
        (self.w.group(&children), (first as u32, last as u32))
    }

    fn array_frames(&mut self, name: &'static str, meta: &str, pod: u32, extent: u32, frames: &[Vec<u8>], layout: Layout) -> P {
        let (group, changed) = self.sample_group(frames, layout, 2);
        P { name, meta: meta.into(), info: 2 | (2 << 2) | (pod << 4) | (extent << 12), group, samples: frames.len() as u32, changed }
    }

    fn scalar_frames(&mut self, name: &'static str, pod: u32, extent: u32, frames: &[Vec<u8>], layout: Layout) -> P {
        let (group, changed) = self.sample_group(frames, layout, 1);
        P { name, meta: String::new(), info: 1 | (2 << 2) | (pod << 4) | (extent << 12), group, samples: frames.len() as u32, changed }
    }

    fn compound(&mut self, name: &'static str, meta: &str, props: Vec<P>) -> P {
        let mut header = Vec::new();
        for p in &props {
            let changed = p.changed != (0, 0);
            let info = p.info | (0xff << 20) | if changed { 0x200 } else { 0 };
            header.extend_from_slice(&info.to_le_bytes());
            if info & 3 != 0 {
                header.extend_from_slice(&p.samples.to_le_bytes());
                if changed {
                    header.extend_from_slice(&p.changed.0.to_le_bytes());
                    header.extend_from_slice(&p.changed.1.to_le_bytes());
                }
            }
            header.extend_from_slice(&(p.name.len() as u32).to_le_bytes());
            header.extend_from_slice(p.name.as_bytes());
            header.extend_from_slice(&(p.meta.len() as u32).to_le_bytes());
            header.extend_from_slice(p.meta.as_bytes());
        }
        let mut children: Vec<u64> = props.iter().map(|p| p.group).collect();
        children.push(self.w.data(&header));
        let group = self.w.group(&children);
        P { name, meta: meta.into(), info: 2 << 2, group, samples: 1, changed: (0, 0) }
    }

    /// An object group: properties and children (name, metadata, group). `indexed` children use
    /// metadata index 1.
    fn object(&mut self, props: Vec<P>, children: &[(&str, &str, u64)]) -> u64 {
        let props = self.compound("", "", props).group;
        let mut header = Vec::new();
        for (name, meta, _) in children {
            header.extend_from_slice(&(name.len() as u32).to_le_bytes());
            header.extend_from_slice(name.as_bytes());
            if meta.is_empty() {
                header.push(1);
            } else {
                header.push(0xff);
                header.extend_from_slice(&(meta.len() as u32).to_le_bytes());
                header.extend_from_slice(meta.as_bytes());
            }
        }
        header.extend_from_slice(&[0; OBJECT_HASH_BYTES]);
        let mut kids = vec![props];
        kids.extend(children.iter().map(|c| c.2));
        kids.push(self.w.data(&header));
        self.w.group(&kids)
    }

    fn archive(mut self, top: u64, indexed_meta: &[&str]) -> Vec<u8> {
        let mut im = Vec::new();
        for m in indexed_meta {
            im.push(m.len() as u8);
            im.extend_from_slice(m.as_bytes());
        }
        let kids = [
            self.w.data(&0i32.to_le_bytes()),
            self.w.data(&10709i32.to_le_bytes()),
            top,
            self.w.data(b"_ai_Application=test"),
            self.w.data(&[]),
            self.w.data(&im),
        ];
        let root = self.w.group(&kids);
        self.w.finish(root)
    }
}

fn f32s(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn i32s(v: &[i32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// A unit quad (two faces of a split square, in face sets "left" / "right") under an Xform
/// translated by (1, 2, 3) and scaled by 2, plus an un-set triangle mesh under the root.
fn sample_file(with_tri: bool) -> Vec<u8> {
    let mut a = Abc { w: Writer::new() };
    // Points of a 2×1 strip in the XY plane, faces wound clockwise seen from +Z (Alembic).
    let p = f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 2.0, 1.0, 0.0]);
    let indices = i32s(&[0, 3, 4, 1, 1, 4, 5, 2]);
    let counts = i32s(&[4, 4]);
    let pp = a.array("P", "interpretation=point", F32, 3, &p);
    let fi = a.array(".faceIndices", "", I32, 1, &indices);
    let fc = a.array(".faceCounts", "", I32, 1, &counts);
    // Indexed face-varying UVs (u = x / 2, v = y).
    let vals = a.array(".vals", "", F32, 2, &f32s(&[0.0, 0.0, 0.5, 0.0, 1.0, 0.0, 0.0, 1.0, 0.5, 1.0, 1.0, 1.0]));
    let uvi = a.array(".indices", "", U32, 1, &i32s(&[0, 3, 4, 1, 1, 4, 5, 2]));
    let uv = a.compound("uv", "geoScope=fvr;podName=float32_t;podExtent=2", vec![vals, uvi]);
    let n = a.array("N", "geoScope=vtx", F32, 3, &f32s(&[0.0, 0.0, 1.0].repeat(6)));
    let geom = a.compound(".geom", "schema=AbcGeom_PolyMesh_v1", vec![pp, fi, fc, uv, n]);
    let left_faces = a.array(".faces", "", I32, 1, &i32s(&[0]));
    let left_fs = a.compound(".faceset", "schema=AbcGeom_FaceSet_v1", vec![left_faces]);
    let left = a.object(vec![left_fs], &[]);
    let right_faces = a.array(".faces", "", I32, 1, &i32s(&[1]));
    let right_fs = a.compound(".faceset", "schema=AbcGeom_FaceSet_v1", vec![right_faces]);
    let right = a.object(vec![right_fs], &[]);
    let fs_meta = "schema=AbcGeom_FaceSet_v1;schemaObjTitle=AbcGeom_FaceSet_v1:.faceset";
    let mesh = a.object(vec![geom], &[("left", fs_meta, left), ("right", fs_meta, right)]);
    // Translate (1, 2, 3) then scale 2: ops apply right to left.
    let ops = a.scalar(".ops", U8, 2, &[0x10, 0x00]);
    let xv: Vec<u8> = [1.0f64, 2.0, 3.0, 2.0, 2.0, 2.0].iter().flat_map(|x| x.to_le_bytes()).collect();
    let vals = a.scalar(".vals", F64, 6, &xv);
    let xform = a.compound(".xform", "schema=AbcGeom_Xform_v3", vec![ops, vals]);
    let xf = a.object(vec![xform], &[("stripShape", "schema=AbcGeom_PolyMesh_v1;schemaObjTitle=AbcGeom_PolyMesh_v1:.geom", mesh)]);
    // A lone triangle without UVs or face sets, metadata through the index.
    let tp = a.array("P", "", F32, 3, &f32s(&[0.0, 0.0, 5.0, 0.0, 1.0, 5.0, 1.0, 0.0, 5.0]));
    let ti = a.array(".faceIndices", "", I32, 1, &i32s(&[0, 1, 2]));
    let tc = a.array(".faceCounts", "", I32, 1, &i32s(&[3]));
    let tgeom = a.compound(".geom", "schema=AbcGeom_PolyMesh_v1", vec![tp, ti, tc]);
    let tri = a.object(vec![tgeom], &[]);
    let mut children = vec![("strip", "schema=AbcGeom_Xform_v3", xf), ("tri", "", tri)];
    children.truncate(if with_tri { 2 } else { 1 });
    let top = a.object(vec![], &children);
    a.archive(top, &["schema=AbcGeom_PolyMesh_v1"])
}

#[test]
fn imports_polymesh_through_xforms_with_face_sets_uvs_and_normals() {
    let raw = read(&sample_file(true)).unwrap();
    let mut names: Vec<(&str, &str)> = raw.groups.iter().map(|(m, p, _)| (m.as_str(), p.as_str())).collect();
    names.sort();
    assert_eq!(names, vec![("Default", "tri"), ("left", "strip"), ("right", "strip")], "meshes take their transform's name");
    assert_eq!(raw.triangle_count(), 5);
    // UVs exist only on the strip, so the whole mesh is unwrapped (with a warning).
    assert!(raw.uvs.is_none() && !raw.warnings.is_empty());
    // World positions: scaled by 2, then translated.
    let (lo, hi) = raw.positions.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
    assert_eq!((lo, hi), (Vec3::new(0.0, 0.0, 3.0), Vec3::new(5.0, 4.0, 5.0)));
    // Clockwise Alembic faces become counter-clockwise: the strip faces +Z.
    for (m, _, tris) in &raw.groups {
        if m == "Default" {
            continue;
        }
        for t in tris.chunks(3) {
            let p = |i: u32| raw.positions[i as usize];
            assert!((p(t[1]) - p(t[0])).cross(p(t[2]) - p(t[0])).z > 0.0);
        }
    }
    let mesh = finish(raw).unwrap().mesh;
    assert_eq!(mesh.materials.len(), 3);
    // The strip alone keeps its indexed face-varying UVs: u = x / 2 in the file's own space.
    let strip = finish(read(&sample_file(false)).unwrap()).unwrap();
    assert!(!strip.report.uvs_generated);
    for (p, uv) in strip.mesh.positions.iter().zip(&strip.mesh.uvs) {
        let local = (*p - Vec3::new(1.0, 2.0, 3.0)) / 2.0;
        assert!((uv.x - local.x / 2.0).abs() < 1e-6 && (uv.y - (1.0 - local.y)).abs() < 1e-6, "{p} {uv}");
    }
}

#[test]
fn strip_alone_keeps_uvs_flipped_to_v_down() {
    let mut a = Abc { w: Writer::new() };
    let pp = a.array("P", "", F32, 3, &f32s(&[0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0]));
    let fi = a.array(".faceIndices", "", I32, 1, &i32s(&[0, 1, 2]));
    let fc = a.array(".faceCounts", "", I32, 1, &i32s(&[3]));
    // Vertex-scope UVs without geoScope metadata: guessed from the count.
    let uv = a.array("uv", "", F32, 2, &f32s(&[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]));
    let geom = a.compound(".geom", "schema=AbcGeom_PolyMesh_v1", vec![pp, fi, fc, uv]);
    let mesh = a.object(vec![geom], &[]);
    let top = a.object(vec![], &[("m", "", mesh)]);
    let bytes = a.archive(top, &["schema=AbcGeom_PolyMesh_v1"]);
    let imported = crate::import_mesh_bytes("m.abc", &bytes).unwrap();
    assert!(!imported.report.uvs_generated);
    let m = &imported.mesh;
    for (p, uv) in m.positions.iter().zip(&m.uvs) {
        assert_eq!(*uv, texturecraft_geom::glam::Vec2::new(p.x, 1.0 - p.y));
    }
}

#[test]
fn hdf5_and_garbage_are_clear_errors() {
    let e = read(b"\x89HDF\r\n\x1a\n rest of an hdf5 file").err().unwrap().to_string();
    assert!(e.contains("HDF5") && e.contains("Ogawa"), "{e}");
    assert!(read(b"not alembic at all").is_err());
    assert!(read(b"").is_err());
}

#[test]
fn truncated_and_corrupted_files_never_panic() {
    let bytes = sample_file(true);
    for n in (0..bytes.len()).step_by(3) {
        let _ = read(&bytes[..n]);
    }
    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..4000 {
        let mut b = bytes.clone();
        for _ in 0..4 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let i = (rng % b.len() as u64) as usize;
            b[i] = (rng >> 32) as u8;
        }
        if let Ok(raw) = read(&b) {
            let _ = finish(raw);
        }
    }
}

#[test]
fn self_referencing_groups_are_bounded() {
    // The top object lists itself as its own child: depth / visit limits stop the walk.
    let mut a = Abc { w: Writer::new() };
    let props = a.compound("", "", vec![]).group;
    let at = a.w.b.len() as u64;
    let mut header = Vec::new();
    header.extend_from_slice(&1u32.to_le_bytes());
    header.push(b'x');
    header.push(0);
    header.extend_from_slice(&[0; OBJECT_HASH_BYTES]);
    // Group: [props, itself, header data] — the header block follows the group.
    let data_at = at + 8 + 3 * 8;
    a.w.group(&[props, at, data_at | DATA_BIT]);
    a.w.data(&header);
    let bytes = a.archive(at, &[]);
    assert!(read(&bytes).is_err());
}

/// A triangle whose `P` sits at z = `heights[frame]`, with constant topology that declares as
/// many samples; with `offsets`, under an Xform translating by (offsets[frame], 0, 0).
fn animated_file(heights: &[f32], offsets: Option<&[f64]>, layout: Layout) -> Vec<u8> {
    let n = heights.len();
    let mut a = Abc { w: Writer::new() };
    let p: Vec<Vec<u8>> = heights.iter().map(|z| f32s(&[0.0, 0.0, *z, 1.0, 0.0, *z, 0.0, 1.0, *z])).collect();
    let pp = a.array_frames("P", "", F32, 3, &p, layout);
    let fi = a.array_frames(".faceIndices", "", I32, 1, &vec![i32s(&[0, 1, 2]); n], layout);
    let fc = a.array_frames(".faceCounts", "", I32, 1, &vec![i32s(&[3]); n], layout);
    let geom = a.compound(".geom", "schema=AbcGeom_PolyMesh_v1", vec![pp, fi, fc]);
    let mesh = a.object(vec![geom], &[]);
    let tri_meta = "schema=AbcGeom_PolyMesh_v1;schemaObjTitle=AbcGeom_PolyMesh_v1:.geom";
    let top = match offsets {
        None => a.object(vec![], &[("tri", tri_meta, mesh)]),
        Some(offsets) => {
            let ops = a.scalar(".ops", U8, 1, &[0x10]);
            let vals: Vec<Vec<u8>> = offsets.iter().map(|x| [*x, 0.0, 0.0].iter().flat_map(|v| v.to_le_bytes()).collect()).collect();
            let vals = a.scalar_frames(".vals", F64, 3, &vals, layout);
            let xform = a.compound(".xform", "schema=AbcGeom_Xform_v3", vec![ops, vals]);
            let xf = a.object(vec![xform], &[("tri", tri_meta, mesh)]);
            a.object(vec![], &[("move", "schema=AbcGeom_Xform_v3", xf)])
        }
    };
    a.archive(top, &["schema=AbcGeom_PolyMesh_v1"])
}

/// Heights of the triangle's points at `frame`, which must agree, and its smallest x.
fn pose(bytes: &[u8], frame: usize) -> (f32, f32) {
    let raw = super::read(bytes, frame).unwrap();
    let z = raw.positions[0].z;
    assert!(raw.positions.iter().all(|p| p.z == z), "{:?}", raw.positions);
    (z, raw.positions.iter().map(|p| p.x).fold(f32::MAX, f32::min))
}

#[test]
fn every_frame_of_an_animated_mesh_can_be_read() {
    // The last two samples repeat: stored once, held for any later frame.
    let bytes = animated_file(&[0.0, 1.0, 2.0, 3.0, 3.0], None, Layout::PerSample);
    for (frame, z) in [(0, 0.0), (1, 1.0), (2, 2.0), (3, 3.0), (4, 3.0), (40, 3.0)] {
        assert_eq!(pose(&bytes, frame).0, z, "frame {frame}");
    }
    let im = crate::import_mesh_bytes_at("a.abc", &bytes, 2).unwrap();
    assert_eq!((im.frames, im.mesh.triangle_count()), (5, 1));
    assert_eq!(im.mesh.positions[0].z, 2.0);
    // The plain import is the first frame.
    assert_eq!(crate::import_mesh_bytes("a.abc", &bytes).unwrap().mesh.positions[0].z, 0.0);
}

#[test]
fn samples_before_the_first_change_can_be_dropped_by_the_writer() {
    // Samples 1 and 2 equal sample 0; the stored ones are 0 and the changes at 3, 4, 5.
    let heights = [0.0, 0.0, 0.0, 5.0, 6.0, 7.0, 7.0];
    let bytes = animated_file(&heights, None, Layout::Compact);
    for (frame, z) in heights.iter().enumerate() {
        assert_eq!(pose(&bytes, frame).0, *z, "compact frame {frame}");
    }
    let bytes = animated_file(&heights, None, Layout::PerSample);
    for (frame, z) in heights.iter().enumerate() {
        assert_eq!(pose(&bytes, frame).0, *z, "per-sample frame {frame}");
    }
}

#[test]
fn animated_transforms_move_a_still_mesh() {
    for layout in [Layout::PerSample, Layout::Compact] {
        let bytes = animated_file(&[1.0, 1.0, 1.0, 1.0], Some(&[0.0, 0.0, 10.0, 20.0]), layout);
        let xs: Vec<f32> = (0..4).map(|f| pose(&bytes, f).1).collect();
        assert_eq!(xs, vec![0.0, 0.0, 10.0, 20.0]);
        assert_eq!(crate::import_mesh_bytes_at("a.abc", &bytes, 3).unwrap().frames, 4);
    }
}

#[test]
fn a_frame_the_file_does_not_have_is_an_error() {
    let still = sample_file(false);
    assert_eq!(crate::import_mesh_bytes_at("a.abc", &still, 0).unwrap().frames, 1);
    let e = crate::import_mesh_bytes_at("a.abc", &still, 1).err().unwrap().to_string();
    assert!(e.contains("frame 1") && e.contains("1 frame"), "{e}");
    let moving = animated_file(&[0.0, 1.0, 2.0], None, Layout::PerSample);
    assert!(crate::import_mesh_bytes_at("a.abc", &moving, 2).is_ok());
    assert!(crate::import_mesh_bytes_at("a.abc", &moving, 3).is_err());
    assert!(crate::import_mesh_bytes_at("a.abc", &moving, usize::MAX).is_err());
    assert!(crate::import_mesh_bytes_at("a.obj", b"v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n", 1).is_err());
}

#[test]
fn corrupted_animated_files_never_panic_at_any_frame() {
    let bytes = animated_file(&[0.0, 1.0, 2.0, 3.0], Some(&[0.0, 1.0, 2.0, 3.0]), Layout::PerSample);
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    for i in 0..3000 {
        let mut b = bytes.clone();
        for _ in 0..4 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let at = (rng % b.len() as u64) as usize;
            b[at] = (rng >> 32) as u8;
        }
        let _ = crate::import_mesh_bytes_at("a.abc", &b, i % 6);
        let _ = super::read(&b[..b.len() - i % 40], usize::MAX - i);
    }
}
