//! The UI state kept between runs as JSON: `ui.json` beside eframe's settings on the desktop, a key of eframe's storage on the web

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::UiState;

/// eframe storage key of the UI state as JSON, where there is no file
const STORAGE_KEY: &str = "ui.json";

/// Name of the UI state in eframe's storage, before the extension, for the copies of unreadable states kept there
const STORAGE_STEM: &str = "ui";

/// eframe storage key under which earlier builds kept the UI state, as RON
const RON_STORAGE_KEY: &str = "ui";

/// Largest UI state file read; a saved state is a few kilobytes
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Copies of unreadable saved states kept before saving stops
const MAX_UNREADABLE_COPIES: u32 = 10;

/// The desktop's `ui.json`, set by the app before its window opens; unset, the state stays in eframe's storage
pub static FILE: OnceLock<PathBuf> = OnceLock::new();

/// Where the UI state is saved, holding a saved state this build could not fully read until it is kept beside it
#[derive(Default)]
pub struct UiStateStore {
    file: Option<PathBuf>,
    /// A saved state this build could not fully read and its file extension, kept by the next save
    unreadable: Option<(Vec<u8>, &'static str)>,
    /// A saved state could not be read or kept, so it is left as it is
    frozen: bool,
}

impl UiStateStore {
    /// The store at `file`, or in eframe's storage without one
    pub fn new(file: Option<PathBuf>) -> Self {
        UiStateStore { file, unreadable: None, frozen: false }
    }

    /// The saved UI state, else the one an earlier build saved; a state not fully read is kept for the next save
    pub fn load(&mut self, storage: Option<&dyn eframe::Storage>) -> Option<UiState> {
        let saved = match &self.file {
            Some(file) if too_large(file) => {
                self.frozen = true;
                return None;
            }
            Some(file) => match std::fs::read(file) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => {
                    self.frozen = true;
                    return None;
                }
            },
            None => storage.and_then(|storage| storage.get_string(STORAGE_KEY)).map(String::into_bytes),
        };
        let Some(bytes) = saved else { return self.load_ron(storage?) };

        let ui = serde_json::from_slice::<UiState>(&bytes).ok();
        if !ui.as_ref().is_some_and(fully_read) {
            self.unreadable = Some((bytes, "json"));
        }
        ui
    }

    /// The RON state an earlier build saved; its docks never read back, so only text that does not parse is kept
    fn load_ron(&mut self, storage: &dyn eframe::Storage) -> Option<UiState> {
        let text = storage.get_string(RON_STORAGE_KEY)?;
        let ui = eframe::get_value::<UiState>(storage, RON_STORAGE_KEY);
        if ui.is_none() {
            self.unreadable = Some((text.into_bytes(), "ron"));
        }
        ui
    }

    /// Save `ui` as JSON once an unreadable saved state is kept, and drop the RON copy of earlier builds
    pub fn save(&mut self, ui: &UiState, storage: &mut dyn eframe::Storage) {
        if let Some((bytes, extension)) = self.unreadable.take() {
            self.frozen = !self.keep_unreadable(&bytes, extension, storage);
        }
        if self.frozen {
            return;
        }

        let Ok(json) = serde_json::to_string_pretty(ui) else { return };
        // JSON writes a non-finite number as null, which would not read back
        if serde_json::from_str::<UiState>(&json).is_err() {
            log::warn!("the UI state was not saved: it would not read back");
            return;
        }

        let saved = match &self.file {
            Some(file) => write_file(file, json.as_bytes()),
            None => {
                storage.set_string(STORAGE_KEY, json);
                true
            }
        };
        if saved {
            storage.remove_string(RON_STORAGE_KEY);
        }
    }

    /// Keep a saved state this build could not fully read under the first free copy name, beside the file or in storage
    fn keep_unreadable(&self, bytes: &[u8], extension: &str, storage: &mut dyn eframe::Storage) -> bool {
        match &self.file {
            Some(file) => keep_file_copy(file, bytes, extension),
            None => {
                let free = (1..=MAX_UNREADABLE_COPIES).map(|n| unreadable_name(STORAGE_STEM, n, extension)).find(|key| storage.get_string(key).is_none());
                let Some(key) = free else { return false };

                storage.set_string(&key, String::from_utf8_lossy(bytes).into_owned());
                true
            }
        }
    }
}

/// The UI state saved in `file`, if it reads
pub fn read(file: &Path) -> Option<UiState> {
    if too_large(file) {
        return None;
    }

    serde_json::from_slice(&std::fs::read(file).ok()?).ok()
}

/// Whether both docks were read rather than replaced by a default
fn fully_read(ui: &UiState) -> bool {
    !ui.dock.replaced_unreadable() && !ui.inactive_mode_dock.replaced_unreadable()
}

/// Whether `file` is too large to be a saved UI state, so it is neither read nor saved over
fn too_large(file: &Path) -> bool {
    std::fs::metadata(file).is_ok_and(|metadata| metadata.len() > MAX_FILE_BYTES)
}

/// The `n`th copy name of an unreadable state: `<stem>.unreadable.<extension>`, then `<stem>.unreadable-N.<extension>`
fn unreadable_name(stem: &str, n: u32, extension: &str) -> String {
    if n == 1 { format!("{stem}.unreadable.{extension}") } else { format!("{stem}.unreadable-{n}.{extension}") }
}

#[cfg(not(target_arch = "wasm32"))]
fn write_file(file: &Path, bytes: &[u8]) -> bool {
    if let Some(folder) = file.parent() {
        let _ = std::fs::create_dir_all(folder);
    }

    texturecraft_engine::formats::write_atomic(file, bytes).is_ok()
}

#[cfg(target_arch = "wasm32")]
fn write_file(_file: &Path, _bytes: &[u8]) -> bool {
    false
}

/// Copy `bytes` beside `file` under the first free copy name
fn keep_file_copy(file: &Path, bytes: &[u8], extension: &str) -> bool {
    use std::io::Write as _;

    let stem = file.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    for n in 1..=MAX_UNREADABLE_COPIES {
        let copy_file = file.with_file_name(unreadable_name(&stem, n, extension));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(copy_file) {
            Ok(mut copy) => return copy.write_all(bytes).is_ok(),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return false,
        }
    }
    false
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::docking::{Dock, Panel, default_workspace, permissions};
    use craft_ui::docking::{Action, Placement};

    /// eframe's storage held in memory
    #[derive(Default)]
    pub(crate) struct MemoryStorage(pub(crate) std::collections::HashMap<String, String>);

    impl eframe::Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }

        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }

        fn remove_string(&mut self, key: &str) {
            self.0.remove(key);
        }

        fn flush(&mut self) {}
    }

    /// Save `ui` and load it back, as closing and reopening the app does
    pub(crate) fn restart(ui: &UiState) -> UiState {
        let mut storage = MemoryStorage::default();
        UiStateStore::default().save(ui, &mut storage);
        UiStateStore::default().load(Some(&storage)).unwrap_or_else(|| panic!("the saved state loads: {:?}", storage.0))
    }

    /// An empty folder of its own for one test
    fn test_folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("texturecraft-ui-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    /// The painting default with Layers moved into a tab beside Assets and Properties floating
    fn moved_panels() -> Dock {
        let mut workspace = default_workspace();
        let actions = [
            Action::Move { panel: Panel::Layers, anchor: Panel::Assets, placement: Placement::Tab { before: None } },
            Action::Float { panel: Panel::Properties, rect: [900.0, 120.0, 320.0, 400.0] },
        ];
        workspace.apply_all(actions, permissions).unwrap();

        let dock = Dock::from_value(serde_json::to_value(&workspace).unwrap());
        assert!(!dock.replaced_unreadable(), "the moved layout is valid");
        dock
    }

    #[test]
    fn panel_layouts_survive_a_restart_in_the_file_and_in_storage() {
        let folder = test_folder("layouts");
        for dock in [Dock::default(), moved_panels()] {
            let ui = UiState { dock, split_ratio: 0.3, ..UiState::default() };
            for file in [None, Some(folder.join("ui.json"))] {
                let mut storage = MemoryStorage::default();
                UiStateStore::new(file.clone()).save(&ui, &mut storage);

                let restarted = UiStateStore::new(file.clone()).load(Some(&storage)).unwrap();
                assert!(fully_read(&restarted), "{file:?}");
                assert_eq!(restarted, ui, "{file:?}");
            }
        }
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn the_state_an_earlier_build_saved_loads_and_its_copy_goes_on_the_next_save() {
        let mut storage = MemoryStorage::default();
        let earlier = UiState { split_ratio: 0.3, ..UiState::default() };
        eframe::set_value(&mut storage, RON_STORAGE_KEY, &earlier);

        let mut store = UiStateStore::default();
        let loaded = store.load(Some(&storage)).expect("the earlier state loads");
        assert_eq!(loaded.split_ratio, 0.3);

        store.save(&loaded, &mut storage);
        assert!(!storage.0.contains_key(RON_STORAGE_KEY));
        assert_eq!(UiStateStore::default().load(Some(&storage)).unwrap(), loaded);
    }

    #[test]
    fn an_unreadable_layout_loads_the_default_and_is_kept_beside_the_file() {
        let folder = test_folder("unreadable");
        let file = folder.join("ui.json");
        let ui = UiState { dock: moved_panels(), split_ratio: 0.3, ..UiState::default() };
        let mut storage = MemoryStorage::default();
        UiStateStore::new(Some(file.clone())).save(&ui, &mut storage);
        let saved = std::fs::read_to_string(&file).unwrap();
        assert!(saved.contains("\"Assets\"") && saved.contains("\"version\": 2"), "{saved}");

        let unreadable = [
            // A panel this build does not know
            saved.replacen("\"Assets\"", "\"Bogus\"", 1),
            // A layout from a newer format
            saved.replacen("\"version\": 2", "\"version\": 99", 1),
            // Not a saved state
            "not json".to_owned(),
        ];
        for (n, text) in unreadable.iter().enumerate() {
            std::fs::write(&file, text).unwrap();
            let mut store = UiStateStore::new(Some(file.clone()));
            let loaded = store.load(Some(&storage));
            if let Some(loaded) = &loaded {
                assert_eq!(loaded.dock, Dock::default(), "{text}");
                assert_eq!(loaded.split_ratio, 0.3, "{text}");
            }

            store.save(&loaded.unwrap_or_default(), &mut storage);
            let copy = unreadable_name("ui", n as u32 + 1, "json");
            assert_eq!(std::fs::read_to_string(folder.join(copy)).unwrap(), *text);
            assert_ne!(std::fs::read_to_string(&file).unwrap(), *text, "the next save replaces it once kept");
        }
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn an_unreadable_state_that_cannot_be_kept_is_not_saved_over() {
        let folder = test_folder("frozen");
        let file = folder.join("ui.json");
        for n in 1..=MAX_UNREADABLE_COPIES {
            std::fs::write(folder.join(unreadable_name("ui", n, "json")), "").unwrap();
        }
        std::fs::write(&file, "not json").unwrap();

        let mut store = UiStateStore::new(Some(file.clone()));
        let mut storage = MemoryStorage::default();
        assert!(store.load(Some(&storage)).is_none());
        store.save(&UiState::default(), &mut storage);
        store.save(&UiState::default(), &mut storage);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "not json");
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_file_too_large_to_read_is_not_saved_over() {
        let folder = test_folder("too-large");
        let file = folder.join("ui.json");
        std::fs::write(&file, vec![b' '; MAX_FILE_BYTES as usize + 1]).unwrap();

        let mut store = UiStateStore::new(Some(file.clone()));
        let mut storage = MemoryStorage::default();
        assert!(store.load(Some(&storage)).is_none());
        store.save(&UiState::default(), &mut storage);
        assert_eq!(std::fs::metadata(&file).unwrap().len(), MAX_FILE_BYTES + 1);
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn unreadable_layouts_in_storage_are_each_kept_under_a_key_of_their_own() {
        let mut storage = MemoryStorage::default();
        UiStateStore::default().save(&UiState { dock: moved_panels(), ..UiState::default() }, &mut storage);
        let saved = storage.0[STORAGE_KEY].clone();

        for (n, panel) in [(1, "\"Bogus\""), (2, "\"Other\"")] {
            let unreadable = saved.replacen("\"Assets\"", panel, 1);
            storage.0.insert(STORAGE_KEY.to_owned(), unreadable.clone());

            let mut store = UiStateStore::default();
            let loaded = store.load(Some(&storage)).unwrap();
            assert_eq!(loaded.dock, Dock::default());
            store.save(&loaded, &mut storage);
            assert_eq!(storage.0[&unreadable_name(STORAGE_STEM, n, "json")], unreadable);
            assert_eq!(UiStateStore::default().load(Some(&storage)).unwrap(), loaded);
        }
        assert_eq!(storage.0[&unreadable_name(STORAGE_STEM, 1, "json")], saved.replacen("\"Assets\"", "\"Bogus\"", 1), "the first copy stays");
    }

    #[test]
    fn an_earlier_state_that_does_not_parse_is_kept_before_its_entry_goes() {
        let folder = test_folder("ron");
        for file in [None, Some(folder.join("ui.json"))] {
            let mut storage = MemoryStorage::default();
            storage.0.insert(RON_STORAGE_KEY.to_owned(), "(not ron".to_owned());

            let mut store = UiStateStore::new(file.clone());
            assert!(store.load(Some(&storage)).is_none());
            store.save(&UiState::default(), &mut storage);
            let copy = match &file {
                Some(_) => std::fs::read_to_string(folder.join(unreadable_name("ui", 1, "ron"))).unwrap(),
                None => storage.0[&unreadable_name(STORAGE_STEM, 1, "ron")].clone(),
            };
            assert_eq!(copy, "(not ron", "{file:?}");
            assert!(!storage.0.contains_key(RON_STORAGE_KEY), "{file:?}");
        }
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_file_that_exists_but_cannot_be_read_is_not_saved_over() {
        let folder = test_folder("read-error");
        let file = folder.join("ui.json");
        // A folder in the file's place fails to read with an error other than not found
        std::fs::create_dir_all(&file).unwrap();
        let mut storage = MemoryStorage::default();
        eframe::set_value(&mut storage, RON_STORAGE_KEY, &UiState::default());

        let mut store = UiStateStore::new(Some(file.clone()));
        assert!(store.load(Some(&storage)).is_none(), "the earlier state is not loaded in its place");
        store.save(&UiState::default(), &mut storage);
        assert!(file.is_dir());
        assert!(storage.0.contains_key(RON_STORAGE_KEY));
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_state_that_would_not_read_back_is_not_saved() {
        let mut storage = MemoryStorage::default();
        let mut store = UiStateStore::default();
        store.save(&UiState { split_ratio: 0.3, ..UiState::default() }, &mut storage);
        let saved = storage.0[STORAGE_KEY].clone();

        store.save(&UiState { uv_pan: [f32::NAN, 0.0], ..UiState::default() }, &mut storage);
        assert_eq!(storage.0[STORAGE_KEY], saved);
    }
}
