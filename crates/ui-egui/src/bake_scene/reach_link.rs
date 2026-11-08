use std::sync::mpsc::{Receiver, TryRecvError};

use rayon::prelude::*;
use texturecraft_engine::Session;
use texturecraft_engine::bake::HighPolyReach;
use texturecraft_engine::core::HighPolySettings;

use super::cage_shell_key::CageShellKey;
use crate::gpu::SceneSync;

/// Each set's (max frontal, max rear) and the selected-set filter
type ShownDistances = (Vec<(f32, f32)>, Option<usize>);

/// The red on the high poly: each texture set's ray reach, worked out on a worker thread, and the distances the GPU's colors show
#[derive(Default)]
pub(super) struct ReachLink {
    /// Each set's ray inputs and high-poly settings (backfaces, name matching), and the id of the high poly, the reach was started for
    keys: Vec<Option<(CageShellKey, HighPolySettings)>>,
    high_id: u64,
    job: Option<Receiver<Vec<Option<HighPolyReach>>>>,
    reach: Vec<Option<HighPolyReach>>,
    /// Each set's (max frontal, max rear) and the selected-set filter the GPU's colors are for
    shown: Option<ShownDistances>,
}

impl ReachLink {
    /// Whether a reach is being worked out (the view must keep drawing to pick it up)
    pub(super) fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// Send the colors again (the GPU lost what it was sent); the reach itself stays
    pub(super) fn resend(&mut self) {
        self.shown = None;
    }

    /// Pick up the reach once the worker finishes; runs every frame, even when nothing is drawn, so `busy` clears
    pub(super) fn poll(&mut self) {
        let Some(job) = &self.job else { return };

        match job.try_recv() {
            Ok(reach) => {
                self.reach = reach;
                self.shown = None;
                self.job = None;
            }
            Err(TryRecvError::Disconnected) => self.job = None,
            Err(TryRecvError::Empty) => {}
        }
    }

    /// Start a new reach when the high poly (sent under `high_id`) or a set's inputs changed, and color the high poly when the reach or the distances changed
    pub(super) fn sync(&mut self, session: &Session, high_id: u64, only_set: Option<usize>, out: &mut SceneSync) {
        let set_count = session.doc.project.texture_sets.len();
        let keys: Vec<Option<(CageShellKey, HighPolySettings)>> = session
            .doc
            .project
            .texture_sets
            .iter()
            .enumerate()
            .map(|(texture_set, set)| Some((CageShellKey::of(session, texture_set)?, set.bake.high_poly.clone())))
            .collect();
        if keys != self.keys || high_id != self.high_id {
            let inputs: Vec<_> = (0..set_count).map(|texture_set| session.bake_cage_inputs(texture_set).ok().flatten()).collect();
            self.job = Some(crate::spawn_worker("texturecraft-bake-reach", move || {
                inputs.iter().map(|inputs| inputs.as_ref().and_then(|inputs| inputs.high_poly_reach().ok())).collect()
            }));
            (self.keys, self.high_id) = (keys, high_id);
        }

        // Colors wait for the reach being worked out: the old ones stay until it arrives
        if self.job.is_some() {
            return;
        }

        let distances: Vec<(f32, f32)> = session.doc.project.texture_sets.iter().map(|set| (set.bake.cage.max_frontal, set.bake.cage.max_rear)).collect();
        let shown = (distances, only_set);
        if self.shown.as_ref() == Some(&shown) {
            return;
        }

        let Some(vertex_count) = session.bake_meshes.merged_high().map(|high| high.mesh.positions.len()) else { return };

        // Each vertex is its nearest set's: other sets, and a set left out by the selected-set filter, never color it
        let sets: Vec<(&HighPolyReach, f32, f32)> = self
            .reach
            .iter()
            .zip(&shown.0)
            .enumerate()
            .filter(|(texture_set, _)| only_set.is_none_or(|only| only == *texture_set))
            .filter_map(|(_, (reach, &(max_frontal, max_rear)))| Some((reach.as_ref()?, max_frontal, max_rear)))
            .collect();
        let missed: Vec<f32> = (0..vertex_count)
            .into_par_iter()
            .map(|vertex| {
                let missed = sets.iter().find_map(|(reach, max_frontal, max_rear)| reach.missed(vertex, *max_frontal, *max_rear));
                f32::from(u8::from(missed.unwrap_or(false)))
            })
            .collect();
        out.bake_high_missed = Some((self.high_id, missed));
        self.shown = Some(shown);
    }
}
