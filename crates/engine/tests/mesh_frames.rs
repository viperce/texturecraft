//! Animated Alembic meshes in a session: `file.setMeshFrame`, the frame in saved projects and
//! reimport, and the refusals (frames out of range, frames that change the topology).

use serde_json::json;
use texturecraft_engine::geom::primitives;
use texturecraft_engine::{Session, execute};

const DATA_BIT: u64 = 1 << 63;
const F32: u32 = 10;
const I32: u32 = 6;

/// A minimal Alembic Ogawa writer: one PolyMesh whose arrays hold a sample per frame.
struct W(Vec<u8>);

struct Prop {
    name: &'static str,
    meta: &'static str,
    pod: u32,
    extent: u32,
    group: u64,
    samples: u32,
    changed: (u32, u32),
    array: bool,
}

impl W {
    fn data(&mut self, b: &[u8]) -> u64 {
        let at = self.0.len() as u64;
        self.0.extend_from_slice(&(b.len() as u64).to_le_bytes());
        self.0.extend_from_slice(b);
        at | DATA_BIT
    }

    fn group(&mut self, kids: &[u64]) -> u64 {
        let at = self.0.len() as u64;
        self.0.extend_from_slice(&(kids.len() as u64).to_le_bytes());
        for k in kids {
            self.0.extend_from_slice(&k.to_le_bytes());
        }
        at
    }

    /// An array property with one sample per entry of `frames` (a sample is stored up to the last
    /// one that differs from its predecessor).
    fn array(&mut self, name: &'static str, meta: &'static str, pod: u32, extent: u32, frames: &[Vec<u8>]) -> Prop {
        let first = (1..frames.len()).find(|i| frames[*i] != frames[0]).unwrap_or(0);
        let last = (1..frames.len()).rev().find(|i| frames[*i] != frames[i - 1]).unwrap_or(0);
        let mut kids = Vec::new();
        for f in &frames[..=last] {
            let mut sample = vec![0u8; 16];
            sample.extend_from_slice(f);
            kids.push(self.data(&sample));
            kids.push(self.data(&[]));
        }
        let group = self.group(&kids);
        Prop { name, meta, pod, extent, group, samples: frames.len() as u32, changed: (first as u32, last as u32), array: true }
    }

    fn compound(&mut self, props: &[Prop]) -> u64 {
        let mut header = Vec::new();
        for p in props {
            let changed = p.changed != (0, 0);
            let kind = if p.array { 2 | (p.pod << 4) | (p.extent << 12) } else { 0 };
            let info: u32 = kind | (2 << 2) | (0xff << 20) | if changed { 0x200 } else { 0 };
            header.extend_from_slice(&info.to_le_bytes());
            if p.array {
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
        let mut kids: Vec<u64> = props.iter().map(|p| p.group).collect();
        kids.push(self.data(&header));
        self.group(&kids)
    }

    /// An object: its property compound, then children (name, metadata, group).
    fn object(&mut self, props: u64, children: &[(&str, &str, u64)]) -> u64 {
        let mut header = Vec::new();
        for (name, meta, _) in children {
            header.extend_from_slice(&(name.len() as u32).to_le_bytes());
            header.extend_from_slice(name.as_bytes());
            header.push(0xff);
            header.extend_from_slice(&(meta.len() as u32).to_le_bytes());
            header.extend_from_slice(meta.as_bytes());
        }
        header.extend_from_slice(&[0; 32]);
        let mut kids = vec![props];
        kids.extend(children.iter().map(|c| c.2));
        kids.push(self.data(&header));
        self.group(&kids)
    }
}

fn f32s(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn i32s(v: &[i32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// One frame of the mesh: points (xyz), triangle corner indices, per-point UVs (uv).
type Frame = (Vec<f32>, Vec<i32>, Vec<f32>);

/// A triangle at height `z`.
fn triangle(z: f32) -> Frame {
    (vec![0.0, 0.0, z, 1.0, 0.0, z, 0.0, 1.0, z], vec![0, 1, 2], vec![0.0, 0.0, 1.0, 0.0, 0.0, 1.0])
}

/// A square of two triangles (a different topology than [`triangle`]).
fn square() -> Frame {
    (vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0], vec![0, 1, 2, 0, 2, 3], vec![0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0])
}

fn alembic(frames: &[Frame]) -> Vec<u8> {
    let mut w = W(vec![0; 16]);
    let col = |f: &dyn Fn(&Frame) -> Vec<u8>| frames.iter().map(f).collect::<Vec<_>>();
    let p = w.array("P", "", F32, 3, &col(&|f| f32s(&f.0)));
    let fi = w.array(".faceIndices", "", I32, 1, &col(&|f| i32s(&f.1)));
    let fc = w.array(".faceCounts", "", I32, 1, &col(&|f| i32s(&vec![3; f.1.len() / 3])));
    let uv = w.array("uv", "geoScope=vtx", F32, 2, &col(&|f| f32s(&f.2)));
    let geom = w.compound(&[p, fi, fc, uv]);
    let geom = Prop { name: ".geom", meta: "schema=AbcGeom_PolyMesh_v1", pod: 0, extent: 0, group: geom, samples: 1, changed: (0, 0), array: false };
    let mesh_props = w.compound(&[geom]);
    let mesh = w.object(mesh_props, &[]);
    let top_props = w.compound(&[]);
    let top = w.object(top_props, &[("tri", "schema=AbcGeom_PolyMesh_v1", mesh)]);
    let kids = [w.data(&0i32.to_le_bytes()), w.data(&10709i32.to_le_bytes()), top, w.data(b"_ai_Application=test"), w.data(&[]), w.data(&[])];
    let root = w.group(&kids);
    w.0[..5].copy_from_slice(b"Ogawa");
    w.0[5] = 0xff;
    w.0[6..8].copy_from_slice(&[0, 1]);
    w.0[8..16].copy_from_slice(&root.to_le_bytes());
    w.0
}

fn session(frames: &[Frame]) -> Session {
    Session::from_mesh_bytes("tri.abc", &alembic(frames), 128, "t").unwrap()
}

fn heights(s: &Session) -> Vec<f32> {
    s.mesh.positions.iter().map(|p| p.z).collect()
}

#[test]
fn the_mesh_follows_the_chosen_frame_and_keeps_its_paint() {
    let mut s = session(&[triangle(0.0), triangle(1.0), triangle(2.0)]);
    assert_eq!((s.mesh_frame, s.mesh_frames), (0, 3));
    assert_eq!(heights(&s), vec![0.0; 3]);
    execute(&mut s, "layer.add.fill", &json!({ "color": [0.8, 0.2, 0.1] })).unwrap();
    let layers = s.doc.project.layers.len();

    let reply = execute(&mut s, "file.setMeshFrame", &json!({ "frame": 2 })).unwrap();
    assert_eq!(reply, json!({ "frame": 2, "frames": 3 }));
    assert_eq!(heights(&s), vec![2.0; 3]);
    assert_eq!(s.doc.project.layers.len(), layers, "the painted layers stay");
    let mesh = &texturecraft_engine::inspect::session(&s)["mesh"];
    assert_eq!((&mesh["frame"], &mesh["frames"]), (&json!(2), &json!(3)));
    assert!(s.modified);

    execute(&mut s, "file.setMeshFrame", &json!({ "frame": 0 })).unwrap();
    assert_eq!(heights(&s), vec![0.0; 3]);
    // The session is fully usable at the new shape: it composites.
    s.update();
}

#[test]
fn the_frame_is_saved_with_the_project_and_kept_by_reimport() {
    let mut s = session(&[triangle(0.0), triangle(1.0), triangle(2.0)]);
    execute(&mut s, "file.setMeshFrame", &json!({ "frame": 1 })).unwrap();
    let back = Session::from_project_bytes(&s.to_project_bytes().unwrap()).unwrap();
    assert_eq!((back.mesh_frame, back.mesh_frames), (1, 3));
    assert_eq!(heights(&back), vec![1.0; 3]);
    s.reimport_mesh(None, false).unwrap();
    assert_eq!((s.mesh_frame, s.mesh_frames), (1, 3));
    assert_eq!(heights(&s), vec![1.0; 3]);
    // A still project saves no frame at all.
    let still = Session::new_project("t", primitives::builtin("plane").unwrap(), "builtin:plane", None, 128).unwrap();
    assert_eq!(still.project_file().project.mesh_frame, 0);
}

#[test]
fn frames_that_cannot_be_used_are_refused_and_change_nothing() {
    let mut s = session(&[triangle(0.0), triangle(1.0), square()]);
    execute(&mut s, "file.setMeshFrame", &json!({ "frame": 1 })).unwrap();
    for (params, wanted) in [
        (json!({ "frame": 3 }), "out of range"),
        (json!({ "frame": 1e30 }), "out of range"),
        (json!({ "frame": 2 }), "topology"),
        (json!({ "frame": -1 }), "frame"),
        (json!({}), "frame"),
    ] {
        let e = execute(&mut s, "file.setMeshFrame", &params).unwrap_err().to_string();
        assert!(e.contains(wanted), "{params}: {e}");
        assert_eq!((s.mesh_frame, heights(&s)), (1, vec![1.0; 3]), "{params}");
    }
    let mut plane = Session::new_project("t", primitives::builtin("plane").unwrap(), "builtin:plane", None, 128).unwrap();
    assert!(execute(&mut plane, "file.setMeshFrame", &json!({ "frame": 0 })).is_err());
    // A still mesh file has one frame.
    let obj = Session::from_mesh_bytes("a.obj", b"v 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 0 1\nf 1/1 2/2 3/3\n", 128, "t").unwrap();
    assert_eq!(obj.mesh_frames, 1);
    let mut obj = obj;
    assert!(execute(&mut obj, "file.setMeshFrame", &json!({ "frame": 1 })).is_err());
    assert!(execute(&mut obj, "file.setMeshFrame", &json!({ "frame": 0 })).is_ok());
}
