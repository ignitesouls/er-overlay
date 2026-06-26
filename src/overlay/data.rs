use serde::Deserialize;
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{Arc, RwLock},
};

use crate::debug_log;

#[derive(Default)]
pub struct AppState {
    pub key_item_quantity: u32,
    pub event_flags: HashMap<i32, bool>,
    pub death_count: u32,
    pub great_runes: i32,
    pub current_events: String,
}

pub type SharedState = Arc<RwLock<AppState>>;

pub fn create_state() -> SharedState {
    Arc::new(RwLock::new(AppState::default()))
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

#[derive(Debug, Deserialize, Clone, Default)]
pub struct EventFlagSchedule {
    #[serde(default)]
    pub schedule_name: Option<String>,

    #[serde(default)]
    pub always_on_flags: Vec<i32>,

    #[serde(default)]
    pub interval_rules: Vec<EventFlagRule>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct EventFlagRule {
    #[serde(default)]
    pub name: Option<String>,

    #[serde(default)]
    pub set_flags_on: Vec<i32>,

    #[serde(default)]
    pub set_flags_off: Vec<i32>,

    #[serde(default)]
    pub flag_labels: HashMap<i32, String>,

    #[serde(default)]
    pub randomize_flags: bool,

    #[serde(default)]
    pub random_min_flags: Option<usize>,

    #[serde(default)]
    pub random_max_flags: Option<usize>,

    pub interval_minutes: u32,

    #[serde(default)]
    pub remove_after_seconds: Option<u32>,

    #[serde(default)]
    pub start_after_seconds: Option<u32>,
}

pub fn load_event_flag_schedule(
    config_dir: &PathBuf,
    event_flag_file: &str,
) -> Option<EventFlagSchedule> {
    let path = config_dir.join(format!("data/{}", event_flag_file));

    if !path.exists() {
        debug_log!(
            "[ignite_overlay] Event flag schedule not found at '{}'",
            path.display()
        );
        return None;
    }

    match fs::read_to_string(&path) {
        Ok(contents) => match serde_json::from_str::<EventFlagSchedule>(&contents) {
            Ok(data) => {
                debug_log!(
                    "[ignite_overlay] Loaded {} event flag interval rules from '{}'",
                    data.interval_rules.len(),
                    path.display()
                );
                Some(data)
            }
            Err(e) => {
                debug_log!(
                    "[ignite_overlay] Failed to parse event flag schedule '{}': {:?}",
                    path.display(),
                    e
                );
                None
            }
        },
        Err(e) => {
            debug_log!(
                "[ignite_overlay] Could not read event flag schedule '{}': {:?}",
                path.display(),
                e
            );
            None
        }
    }
}

pub fn load_localized_boss_data(
    config_dir: &PathBuf,
    language: &str,
    data_file: &str,
) -> Option<BossRegions> {
    let lang_norm = language.trim();
    let lang_norm = if lang_norm.is_empty() {
        "engus"
    } else {
        lang_norm
    };

    let candidates = [
        config_dir.join(format!("data/{}/{}", lang_norm, data_file)),
        config_dir.join(format!("data/engus/{}", data_file)),
        config_dir.join(format!("data/{}", data_file)),
    ];

    for path in candidates {
        if !path.exists() {
            debug_log!(
                "[ignite_overlay] Boss data not found at '{}'",
                path.display()
            );
            continue;
        }

        match fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<BossRegions>(&contents) {
                Ok(data) => {
                    debug_log!(
                        "[ignite_overlay] Loaded boss data from '{}'",
                        path.display()
                    );
                    return Some(data);
                }
                Err(e) => {
                    debug_log!(
                        "[ignite_overlay] Failed to parse JSON '{}': {:?}",
                        path.display(),
                        e
                    );
                }
            },
            Err(e) => {
                debug_log!(
                    "[ignite_overlay] Could not read '{}': {:?}",
                    path.display(),
                    e
                );
            }
        }
    }

    debug_log!("[ignite_overlay] No valid boss data file found");
    None
}
