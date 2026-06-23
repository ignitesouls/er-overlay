use std::{
    fs,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use eldenring::cs::{BlockId, CSEventFlagMan, WorldChrMan};
use eldenring::position::BlockPosition;
use fromsoftware_shared::singleton::get_instance;
use serde::Deserialize;

use crate::{debug_log, util::introspection::get_dll_directory};

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct JsonMapId {
    pub area: u8,
    pub block: u8,
    pub region: u8,
    pub layer: u8,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GraceEntry {
    pub name: String,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub map_id: Option<JsonMapId>,
    pub discovery_flag: u32,
}

const GRACE_RADIUS: f32 = 3.0;
const GRACE_RADIUS_SQ: f32 = GRACE_RADIUS * GRACE_RADIUS;
const MAX_VERTICAL_DELTA: f32 = 3.0;

const GRACE_JSON_PATH: &str = "graces.json";

static GRACES_CACHE: OnceLock<Mutex<Vec<GraceEntry>>> = OnceLock::new();

#[inline(always)]
pub const fn map_block_id(area: i32, block: i32, region: i32) -> BlockId {
    BlockId((area << 24) | (block << 16) | (region << 8))
}

impl GraceEntry {
    #[inline(always)]
    pub fn pos(&self) -> BlockPosition {
        BlockPosition::from_xyz(self.x, self.y, self.z)
    }

    #[inline(always)]
    pub fn packed_block_id(&self) -> Option<BlockId> {
        self.map_id
            .map(|m| map_block_id(m.area as i32, m.block as i32, m.region as i32))
    }
}

#[inline(always)]
fn dist_sq(a: BlockPosition, b: BlockPosition) -> f32 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    let dz = a.z - b.z;
    dx * dx + dy * dy + dz * dz
}

fn grace_cache() -> &'static Mutex<Vec<GraceEntry>> {
    GRACES_CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

fn candidate_grace_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    paths.push(PathBuf::from(GRACE_JSON_PATH));
    paths.push(PathBuf::from("data").join(GRACE_JSON_PATH));
    paths.push(PathBuf::from("config").join(GRACE_JSON_PATH));

    if let Some(dll_dir) = get_dll_directory() {
        paths.push(dll_dir.join(GRACE_JSON_PATH));
        paths.push(dll_dir.join("data").join(GRACE_JSON_PATH));
        paths.push(dll_dir.join("config").join(GRACE_JSON_PATH));
    }

    paths
}

fn load_graces_from_disk() -> Result<Vec<GraceEntry>, String> {
    let mut errors = Vec::new();

    for path in candidate_grace_paths() {
        match fs::read_to_string(&path) {
            Ok(text) => {
                let parsed: Vec<GraceEntry> = serde_json::from_str(&text)
                    .map_err(|e| format!("failed to parse {}: {}", path.display(), e))?;

                debug_log!(
                    "[grace] loaded {} graces from {}",
                    parsed.len(),
                    path.display()
                );

                return Ok(parsed);
            }
            Err(e) => {
                errors.push(format!("{} -> {}", path.display(), e));
            }
        }
    }

    Err(format!(
        "could not load graces.json from any path: {}",
        errors.join(" | ")
    ))
}

pub fn reload_graces_from_json() {
    match load_graces_from_disk() {
        Ok(new_graces) => {
            let cache = grace_cache();
            let mut guard = cache.lock().unwrap();
            *guard = new_graces;
            debug_log!("[grace] grace cache reloaded, count={}", guard.len());
        }
        Err(e) => {
            debug_log!("[grace] reload failed: {}", e);
        }
    }
}

fn ensure_graces_loaded() {
    let cache = grace_cache();

    {
        let guard = cache.lock().unwrap();
        if !guard.is_empty() {
            return;
        }
    }

    match load_graces_from_disk() {
        Ok(new_graces) => {
            let mut guard = cache.lock().unwrap();
            *guard = new_graces;
            debug_log!("[grace] initial load success, count={}", guard.len());
        }
        Err(e) => {
            debug_log!("[grace] initial load failed: {}", e);
        }
    }
}

pub fn try_auto_activate_nearby_grace() {
    ensure_graces_loaded();

    let Some(world_chr_man) = (unsafe { get_instance::<WorldChrMan>() }) else {
        return;
    };

    let Some(player_ptr) = &world_chr_man.main_player else {
        return;
    };

    let player = unsafe { player_ptr.as_ref() };
    let player_pos = player.block_position;
    let player_block_id = player.current_block_id;

    let Some(flag_man) = (unsafe { get_instance::<CSEventFlagMan>() }) else {
        return;
    };

    let cache = grace_cache();
    let guard = cache.lock().unwrap();

    for grace in guard.iter() {
        let grace_pos = grace.pos();

        if let Some(required_block_id) = grace.packed_block_id() {
            if player_block_id != required_block_id {
                continue;
            }
        }

        let distance_sq = dist_sq(player_pos, grace_pos);
        let vertical_delta = (player_pos.y - grace_pos.y).abs();

        if distance_sq > GRACE_RADIUS_SQ {
            continue;
        }

        if vertical_delta > MAX_VERTICAL_DELTA {
            continue;
        }

        let already_discovered = flag_man.virtual_memory_flag.get_flag(grace.discovery_flag);

        if !already_discovered {
            unsafe {
                let flag_man_ptr = flag_man as *const CSEventFlagMan as *mut CSEventFlagMan;
                (*flag_man_ptr)
                    .virtual_memory_flag
                    .set_flag(grace.discovery_flag, true);
            }

            debug_log!(
                "[grace] discovered '{}' pos=({:.2}, {:.2}, {:.2}) player=({:.2}, {:.2}, {:.2}) block={:?} flag={}",
                grace.name,
                grace_pos.x,
                grace_pos.y,
                grace_pos.z,
                player_pos.x,
                player_pos.y,
                player_pos.z,
                player_block_id,
                grace.discovery_flag
            );
        } else {
            debug_log!(
                "[grace] near already discovered '{}' block={:?} flag={}",
                grace.name,
                player_block_id,
                grace.discovery_flag
            );
        }

        break;
    }
}

pub fn log_player_grace_debug() {
    let Some(world_chr_man) = (unsafe { get_instance::<WorldChrMan>() }) else {
        return;
    };

    let Some(player_ptr) = &world_chr_man.main_player else {
        return;
    };

    let player = unsafe { player_ptr.as_ref() };

    debug_log!(
        "[grace_debug] pos=({:.2}, {:.2}, {:.2}) block={:?}",
        player.block_position.x,
        player.block_position.y,
        player.block_position.z,
        player.current_block_id
    );
}

pub fn log_loaded_graces_debug() {
    ensure_graces_loaded();

    let cache = grace_cache();
    let guard = cache.lock().unwrap();

    debug_log!("[grace] loaded grace count={}", guard.len());

    for grace in guard.iter() {
        debug_log!(
            "[grace] '{}' pos=({:.2}, {:.2}, {:.2}) block={:?} flag={}",
            grace.name,
            grace.x,
            grace.y,
            grace.z,
            grace.packed_block_id(),
            grace.discovery_flag
        );
    }
}

pub fn find_grace_by_name(name: &str) -> Option<GraceEntry> {
    ensure_graces_loaded();

    let cache = grace_cache();
    let guard = cache.lock().unwrap();

    guard
        .iter()
        .find(|g| g.name.eq_ignore_ascii_case(name))
        .cloned()
}

pub fn find_grace_by_discovery_flag(flag: u32) -> Option<GraceEntry> {
    ensure_graces_loaded();

    let cache = grace_cache();
    let guard = cache.lock().unwrap();

    guard.iter().find(|g| g.discovery_flag == flag).cloned()
}
