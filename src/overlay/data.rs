use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    sync::{Arc, RwLock},
};

use crate::debug_log;

#[derive(Default)]
pub struct AppState {
    pub key_item_quantity: u32,
    pub event_flags: HashMap<i32, bool>,
    pub initialized: bool,
    pub death_count: u32,
    pub great_runes: i32,

    // phase / region tracking
    pub active_phase_index: Option<usize>,
    pub active_region_name: String,

    // current displayed counters
    pub counted_kills: u32,
    pub counted_total: u32,

    // persistent strict counting state
    pub counted_flags: HashSet<i32>,
    pub cumulative_counted_kills: u32,

    // optional timing/debug info
    pub boss_first_kill_time: HashMap<i32, u32>,

    // prevents phase entry rewards/events from firing more than once
    pub fired_enter_once: HashSet<usize>,
}

pub type SharedState = Arc<RwLock<AppState>>;

pub fn create_state() -> SharedState {
    Arc::new(RwLock::new(AppState::default()))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PersistedRunState {
    pub seed: String,

    #[serde(default)]
    pub counted_flags: HashSet<i32>,

    #[serde(default)]
    pub cumulative_counted_kills: u32,
}

pub fn save_run_state(config_dir: &PathBuf, state: &PersistedRunState) -> bool {
    let path = config_dir.join("data/run_progress.json");

    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            debug_log!(
                "[ignite_overlay] ❌ Failed creating progress dir '{}': {:?}",
                parent.display(),
                e
            );
            return false;
        }
    }

    match serde_json::to_string_pretty(state) {
        Ok(json) => match fs::write(&path, json) {
            Ok(_) => {
                debug_log!(
                    "[ignite_overlay] ✅ Saved run progress to '{}'",
                    path.display()
                );
                true
            }
            Err(e) => {
                debug_log!(
                    "[ignite_overlay] ❌ Failed writing run progress '{}': {:?}",
                    path.display(),
                    e
                );
                false
            }
        },
        Err(e) => {
            debug_log!(
                "[ignite_overlay] ❌ Failed serializing run progress: {:?}",
                e
            );
            false
        }
    }
}

pub fn load_run_state(config_dir: &PathBuf, seed: &str) -> Option<PersistedRunState> {
    let path = config_dir.join("data/run_progress.json");

    if !path.exists() {
        debug_log!(
            "[ignite_overlay] ℹ No run progress file found at '{}'",
            path.display()
        );
        return None;
    }

    let contents = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            debug_log!(
                "[ignite_overlay] ❌ Failed reading run progress '{}': {:?}",
                path.display(),
                e
            );
            return None;
        }
    };

    let saved = match serde_json::from_str::<PersistedRunState>(&contents) {
        Ok(s) => s,
        Err(e) => {
            debug_log!(
                "[ignite_overlay] ❌ Failed parsing run progress '{}': {:?}",
                path.display(),
                e
            );
            return None;
        }
    };

    if saved.seed.trim() == seed.trim() {
        debug_log!(
            "[ignite_overlay] ✅ Loaded run progress for seed '{}'",
            seed
        );
        Some(saved)
    } else {
        debug_log!(
            "[ignite_overlay] ℹ Run progress seed mismatch: file='{}' current='{}' — starting fresh",
            saved.seed,
            seed
        );
        None
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct BossEntry {
    pub boss: String,
    pub place: String,
    pub flag_id: i32,

    #[serde(default)]
    pub remembrance: Option<i32>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RegionData {
    pub region_name: String,

    #[serde(default)]
    pub regions: Vec<i32>,

    #[serde(default)]
    pub bosses: Vec<BossEntry>,
}

pub type BossRegions = Vec<RegionData>;

#[derive(Debug, Deserialize, Clone)]
pub struct RegionSchedule {
    pub schedule_name: String,
    pub count_mode: String,
    pub time_basis: String,
    pub phases: Vec<SchedulePhase>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SchedulePhase {
    pub name: String,
    pub region_name: String,
    pub duration_minutes: u64,

    #[serde(default)]
    pub on_enter_once: Option<PhaseActionSet>,

    #[serde(default)]
    pub while_active: Option<PhaseActionSet>,

    #[serde(default)]
    pub on_exit: Option<PhaseActionSet>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct PhaseActionSet {
    #[serde(default)]
    pub set_flags_on: Vec<i32>,

    #[serde(default)]
    pub set_flags_off: Vec<i32>,
}

pub fn load_localized_boss_data(
    config_dir: &PathBuf,
    language: &str,
    data_file: &str,
) -> Option<BossRegions> {
    let lang_norm = language.trim();
    let lang_norm = if lang_norm.is_empty() { "engus" } else { lang_norm };

    let candidates = [
        config_dir.join(format!("data/{}/{}", lang_norm, data_file)),
        config_dir.join(format!("data/engus/{}", data_file)),
        config_dir.join(format!("data/{}", data_file)),
    ];

    for path in candidates {
        if !path.exists() {
            debug_log!(
                "[ignite_overlay] ⚠ Boss data not found at '{}'",
                path.display()
            );
            continue;
        }

        match fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<BossRegions>(&contents) {
                Ok(data) => {
                    debug_log!(
                        "[ignite_overlay] ✅ Loaded boss data from '{}'",
                        path.display()
                    );
                    return Some(data);
                }
                Err(e) => {
                    debug_log!(
                        "[ignite_overlay] ❌ Failed to parse JSON '{}': {:?}",
                        path.display(),
                        e
                    );
                }
            },
            Err(e) => {
                debug_log!(
                    "[ignite_overlay] ❌ Could not read '{}': {:?}",
                    path.display(),
                    e
                );
            }
        }
    }

    debug_log!("[ignite_overlay] ❌ No valid boss data file found");
    None
}

pub fn load_region_schedule(config_dir: &PathBuf, schedule_file: &str) -> Option<RegionSchedule> {
    let path = config_dir.join(format!("data/{}", schedule_file));

    if !path.exists() {
        debug_log!(
            "[ignite_overlay] ⚠ Region schedule not found at '{}'",
            path.display()
        );
        return None;
    }

    match fs::read_to_string(&path) {
        Ok(contents) => match serde_json::from_str::<RegionSchedule>(&contents) {
            Ok(data) => {
                debug_log!(
                    "[ignite_overlay] ✅ Loaded region schedule from '{}'",
                    path.display()
                );
                Some(data)
            }
            Err(e) => {
                debug_log!(
                    "[ignite_overlay] ❌ Failed to parse schedule JSON '{}': {:?}",
                    path.display(),
                    e
                );
                None
            }
        },
        Err(e) => {
            debug_log!(
                "[ignite_overlay] ❌ Could not read schedule '{}': {:?}",
                path.display(),
                e
            );
            None
        }
    }
}