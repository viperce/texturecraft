//! A bake job belongs to the mesh it started on: when Edit > Change mesh (or a reimport) swaps
//! the mesh meanwhile, the job is cancelled and its result is dropped.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use egui_kittest::Harness;
use texturecraft_engine::bake::Progress;
use texturecraft_ui_egui::{BakeJob, TextureCraftApp};

/// An app with a running bake job (its worker never answers) started on mesh `mesh_id`.
fn harness_with_job(mesh_id: impl FnOnce(u64) -> u64) -> (Harness<'static, TextureCraftApp>, Arc<Progress>) {
    let session = texturecraft_engine::sample_session(256, 64).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 960.0)).build_eframe(|cc| TextureCraftApp::new(cc, session));
    h.run_steps(4);
    let progress = Arc::new(Progress::new());
    let (tx, rx) = std::sync::mpsc::channel();
    let id = mesh_id(h.state().session.mesh_id);
    // Leak the sender so the channel stays empty instead of closed.
    std::mem::forget(tx);
    h.state_mut().bake = Some(BakeJob { queue: vec![], current: Some((0, progress.clone(), rx)), total: 1, mesh_id: id });
    (h, progress)
}

#[test]
fn a_job_on_a_swapped_mesh_is_cancelled_and_dropped() {
    let (mut h, progress) = harness_with_job(|current| current + 1);
    h.step();
    assert!(h.state().bake.is_none(), "the job is dropped");
    assert!(progress.cancel.load(Ordering::Relaxed), "and its worker is told to stop");
}

#[test]
fn a_job_on_the_current_mesh_keeps_running() {
    let (mut h, progress) = harness_with_job(|current| current);
    h.step();
    assert!(h.state().bake.is_some(), "the job stays");
    assert!(!progress.cancel.load(Ordering::Relaxed));
}
