//! Native file dialogs. On macOS they are sheets: a dialog run modally inside the window's event
//! handler gets the window events of its own resizing sent back into that handler, and winit
//! aborts the app. A sheet returns at once and the window keeps running; a thread of its own waits
//! for the answer. Elsewhere the dialog blocks where it is asked for. The browser has no native
//! dialogs (web_files.rs): asking there does nothing.
//!
//! [`Picker::ask`] shows a dialog for a `tag` naming what asked for it; the answer is
//! [`Picker::take`]n (with its tag) in the same frame, or a later one on macOS.

use std::path::PathBuf;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc::{Receiver, TryRecvError};

/// The pseudo-tags of dialogs are free text; [`crate::windows::picked`] reads them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Open,
    OpenMany,
    /// With the suggested file name.
    Save,
    Folder,
}

/// A dialog to show: what it picks, its file types (label, extensions), the folder to start in and
/// the suggested file name.
pub struct Request {
    kind: Kind,
    filters: Vec<(String, Vec<String>)>,
    dir: Option<PathBuf>,
    name: Option<String>,
}

impl Request {
    pub fn new(kind: Kind) -> Self {
        Request { kind, filters: Vec::new(), dir: None, name: None }
    }

    pub fn filter(mut self, label: impl Into<String>, exts: &[impl AsRef<str>]) -> Self {
        self.filters.push((label.into(), exts.iter().map(|e| e.as_ref().to_string()).collect()));
        self
    }

    pub fn dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.dir = Some(dir.into());
        self
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// The paths a dialog picked, for the tag that asked for it.
pub type Picked = (String, Vec<PathBuf>);

/// The open dialog, if any, and what it picked.
#[derive(Default)]
pub struct Picker {
    #[cfg(not(target_arch = "wasm32"))]
    pending: Option<(String, Receiver<Vec<PathBuf>>)>,
    picked: Option<Picked>,
    /// The window to wake when a sheet answers, from the last [`Self::poll`].
    ctx: Option<egui::Context>,
}

impl Picker {
    /// Shows `request` for `tag`: one dialog at a time.
    pub fn ask(&mut self, tag: impl Into<String>, request: Request) {
        #[cfg(target_arch = "wasm32")]
        let _ = (tag, request);
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.pending.is_some() {
                return;
            }
            let tag = tag.into();
            #[cfg(target_os = "macos")]
            match sheet(request, self.ctx.clone()) {
                Some(answer) => self.pending = Some((tag, answer)),
                None => log::warn!("no file dialog could be shown"),
            }
            #[cfg(not(target_os = "macos"))]
            {
                let paths = blocking(request);
                self.picked = (!paths.is_empty()).then_some((tag, paths));
            }
        }
    }

    /// Collects the answer of the open dialog, once it has one.
    pub fn poll(&mut self, ctx: &egui::Context) {
        self.ctx = Some(ctx.clone());
        #[cfg(not(target_arch = "wasm32"))]
        if let Some((tag, answer)) = &self.pending {
            match answer.try_recv() {
                Err(TryRecvError::Empty) => {}
                Ok(paths) => {
                    self.picked = (!paths.is_empty()).then(|| (tag.clone(), paths));
                    self.pending = None;
                }
                Err(TryRecvError::Disconnected) => self.pending = None,
            }
        }
    }

    /// The tag and paths of a dialog that picked something, once.
    pub fn take(&mut self) -> Option<Picked> {
        self.picked.take()
    }

    /// Whether a dialog is open.
    pub fn open(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        return self.pending.is_some();
        #[cfg(target_arch = "wasm32")]
        false
    }
}

#[cfg(not(target_os = "macos"))]
#[cfg(not(target_arch = "wasm32"))]
fn blocking(request: Request) -> Vec<PathBuf> {
    let mut dialog = request.filters.iter().fold(rfd::FileDialog::new(), |d, (label, exts)| d.add_filter(label, exts));
    if let Some(dir) = request.dir {
        dialog = dialog.set_directory(dir);
    }
    if let Some(name) = request.name {
        dialog = dialog.set_file_name(name);
    }
    match request.kind {
        Kind::Open => dialog.pick_file().into_iter().collect(),
        Kind::OpenMany => dialog.pick_files().unwrap_or_default(),
        Kind::Save => dialog.save_file().into_iter().collect(),
        Kind::Folder => dialog.pick_folder().into_iter().collect(),
    }
}

/// The sheet is begun here, on the main thread (as rfd needs); a thread waits for its answer.
#[cfg(target_os = "macos")]
fn sheet(request: Request, ctx: Option<egui::Context>) -> Option<Receiver<Vec<PathBuf>>> {
    type Answer = std::pin::Pin<Box<dyn std::future::Future<Output = Vec<PathBuf>> + Send>>;
    let (sheet_tx, sheet_rx) = std::sync::mpsc::channel::<Answer>();
    let (tx, rx) = std::sync::mpsc::channel();
    // The thread first, so the answer of a sheet once shown always has somewhere to go.
    std::thread::Builder::new()
        .name("file-dialog".into())
        .spawn(move || {
            if let Ok(answer) = sheet_rx.recv() {
                // The app gone meanwhile has no use for the answer.
                let _ = tx.send(pollster::block_on(answer));
                if let Some(ctx) = ctx {
                    ctx.request_repaint();
                }
            }
        })
        .ok()?;
    let mut dialog = request.filters.iter().fold(rfd::AsyncFileDialog::new(), |d, (label, exts)| d.add_filter(label, exts));
    if let Some(dir) = request.dir {
        dialog = dialog.set_directory(dir);
    }
    if let Some(name) = request.name {
        dialog = dialog.set_file_name(name);
    }
    let path = |f: rfd::FileHandle| f.path().to_path_buf();
    let answer: Answer = match request.kind {
        Kind::Open => {
            let picked = dialog.pick_file();
            Box::pin(async move { picked.await.map(path).into_iter().collect() })
        }
        Kind::OpenMany => {
            let picked = dialog.pick_files();
            Box::pin(async move { picked.await.unwrap_or_default().into_iter().map(path).collect() })
        }
        Kind::Save => {
            let picked = dialog.save_file();
            Box::pin(async move { picked.await.map(path).into_iter().collect() })
        }
        Kind::Folder => {
            let picked = dialog.pick_folder();
            Box::pin(async move { picked.await.map(path).into_iter().collect() })
        }
    };
    let _ = sheet_tx.send(answer);
    Some(rx)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// A dialog that has not answered is open and picks nothing yet; its answer is taken once.
    #[test]
    fn an_answer_is_taken_once_after_it_arrives() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut picker = Picker { pending: Some(("open".into(), rx)), picked: None, ctx: None };
        picker.poll(&egui::Context::default());
        assert!(picker.open() && picker.take().is_none(), "no answer yet");
        tx.send(vec![PathBuf::from("/tmp/a.tcraft")]).unwrap();
        picker.poll(&egui::Context::default());
        assert!(!picker.open());
        assert_eq!(picker.take(), Some(("open".to_string(), vec![PathBuf::from("/tmp/a.tcraft")])));
        assert!(picker.take().is_none());
    }

    /// A cancelled dialog (no paths) picks nothing.
    #[test]
    fn a_cancelled_dialog_picks_nothing() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut picker = Picker { pending: Some(("open".into(), rx)), picked: None, ctx: None };
        tx.send(Vec::new()).unwrap();
        picker.poll(&egui::Context::default());
        assert!(!picker.open() && picker.take().is_none());
    }
}
