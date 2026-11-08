//! Import mesh maps baked in another app: many files at once, each to the set and slot its file name names

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};
use texturecraft_core::BakedMap;

use crate::commands::{CommandSpec, always, bad, strp};
use crate::meshmap_cmds::{FIRST_TILE, tile_number, use_images};
use crate::meshmap_file_name::{baked_map_from_file_name, texture_set_from_file_name};
use crate::resource_cmds::read_image_file;
use crate::{Result, Session, cmd};

const CMD: &str = "textureSet.meshMap.import";

/// Most files one import takes: every mesh map of dozens of sets, each split over a few UV tiles
const MAX_FILES: usize = 512;

/// Most bytes one import reads, since every file stays decoded in memory until the import ends
const MAX_TOTAL_BYTES: u64 = 2 << 30;

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "textureSet.meshMap.import",
        "Import baked maps",
        [],
        None,
        "{files: [{path, map?: mesh map id}]} → {assigned: [{path, ts, map}], unmatched: [path]} — each image file becomes the mesh map its name names (`Body_ao.png`, `Body_world_normal.png`; or `map`) of the texture set its name starts with (any name when there is one set), read as by textureSet.meshMap.set. A UV-tiled set takes one file per tile, named with the tile number (`Body_ao.1001.png`, `Body_ao.1002.png`). Left out and listed in `unmatched`: files matching no set or map, a second file for one slot or tile, files of a UV-tiled set without one of its tile numbers, and every file of a map missing a tile. One undo step (none when nothing matches); desktop only",
        always,
        import
    )]
}

fn import(s: &mut Session, p: &Value) -> Result<Value> {
    let files = p.get("files").and_then(Value::as_array).ok_or_else(|| bad(CMD, "`files` (a list of {path, map?}) is required"))?;
    if files.len() > MAX_FILES {
        return Err(bad(CMD, format!("at most {MAX_FILES} files at once")));
    }

    let set_names: Vec<&str> = s.doc.project.texture_sets.iter().map(|set| set.name.as_str()).collect();
    let mut slots: BTreeMap<(usize, BakedMap), BTreeMap<u32, &str>> = BTreeMap::new();
    let mut unmatched = Vec::new();
    for file in files {
        let path = strp(file, "path").ok_or_else(|| bad(CMD, "every file needs a `path`"))?;
        let given_map = strp(file, "map").map(|id| BakedMap::parse(id).ok_or_else(|| bad(CMD, format!("unknown mesh map `{id}`")))).transpose()?;
        match slot_of(s, &set_names, path, given_map) {
            Some((slot, tile)) if !slots.get(&slot).is_some_and(|tiles| tiles.contains_key(&tile)) => {
                slots.entry(slot).or_default().insert(tile, path);
            }
            _ => unmatched.push(path),
        }
    }

    // A UV-tiled map needs a file for every tile of its set
    slots.retain(|(ts, _), tiles| {
        let complete =
            s.doc.project.texture_sets.get(*ts).is_some_and(|set| set.tile_grid().is_none() || set.uv_tiles.iter().all(|tile| tiles.contains_key(tile)));
        if !complete {
            unmatched.extend(tiles.values());
        }
        complete
    });
    if slots.is_empty() {
        return Ok(json!({ "assigned": [], "unmatched": unmatched }));
    }

    let result = s.edit("Import baked maps", |s| {
        let mut assigned = Vec::new();
        let mut total_bytes = 0;
        for ((ts, map), tiles) in &slots {
            let mut images = BTreeMap::new();
            for (tile, path) in tiles {
                let bytes = read_image_file(CMD, path)?;
                total_bytes += bytes.len() as u64;
                if total_bytes > MAX_TOTAL_BYTES {
                    return Err(bad(CMD, format!("the files are larger than {} GB together", MAX_TOTAL_BYTES >> 30)));
                }

                let name = s.add_image_resource(path, bytes)?;
                if let Some(image) = s.images.get(&name) {
                    images.insert(*tile, image.clone());
                }
                assigned.push(json!({"path": path, "ts": ts, "map": map.id()}));
            }
            use_images(s, CMD, *ts, *map, &images)?;
        }
        Ok(assigned)
    });
    // A failed edit leaves the document as it was; forget the images it decoded
    if result.is_err() {
        s.sync_images();
    }

    Ok(json!({ "assigned": result?, "unmatched": unmatched }))
}

/// The texture set and mesh map a file is for, and its UV tile ([`FIRST_TILE`] on a set without UV tiles)
fn slot_of(s: &Session, set_names: &[&str], path: &str, given_map: Option<BakedMap>) -> Option<((usize, BakedMap), u32)> {
    let ts = texture_set_from_file_name(path, set_names)?;
    let map = given_map.or_else(|| baked_map_from_file_name(path, set_names.get(ts).copied().unwrap_or_default()))?;
    let texture_set = s.doc.project.texture_sets.get(ts)?;
    let tile = match texture_set.tile_grid() {
        Some(_) => tile_number(Path::new(path).file_name()?.to_str()?).filter(|tile| texture_set.uv_tiles.contains(tile))?,
        None => FIRST_TILE,
    };
    Some(((ts, map), tile))
}
