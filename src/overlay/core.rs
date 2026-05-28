use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    thread,
    time::Duration,
};

use std::path::PathBuf;

use eldenring_util::system::wait_for_system_init;
use fromsoftware_shared::program::Program;

use crate::{
    debug_log,
    overlay::data::{
        save_run_state, BossRegions, PersistedRunState, PhaseActionSet, RegionSchedule,
        SharedState,
    },
    er::{
        events::{
            build_cache, read_entry_size, read_from_flag_location, read_root,
            try_resolve_eventflagman, write_flag,
        },
        gamedata::{read_death_count, read_in_game_time, try_resolve_gamedataman},
        inventory::get_key_item_quantity,
        stats::{try_apply_region_stats, RegionStatProfile},
    },
};

fn active_phase_index(schedule: &RegionSchedule, igt_seconds: u32) -> Option<usize> {
    let mut start = 0u32;

    for (idx, phase) in schedule.phases.iter().enumerate() {
        let end = start + (phase.duration_minutes as u32) * 60;
        if igt_seconds >= start && igt_seconds < end {
            return Some(idx);
        }
        start = end;
    }

    None
}

fn region_stat_profile(region_name: &str) -> Option<RegionStatProfile> {
    match region_name {
        "Limgrave" => Some(RegionStatProfile {
            vigor: 30,
            mind: 10,
            endurance: 15,
            strength: 10,
            dexterity: 10,
            intelligence: 10,
            faith: 10,
            arcane: 10,
        }),
        "Liurnia of the Lakes" => Some(RegionStatProfile {
            vigor: 40,
            mind: 30,
            endurance: 12,
            strength: 8,
            dexterity: 12,
            intelligence: 60,
            faith: 10,
            arcane: 10,
        }),
        "Caelid" => Some(RegionStatProfile {
            vigor: 50,
            mind: 10,
            endurance: 20,
            strength: 50,
            dexterity: 18,
            intelligence: 1,
            faith: 1,
            arcane: 50,
        }),
        "Altus Plateau" => Some(RegionStatProfile {
            vigor: 50,
            mind: 20,
            endurance: 20,
            strength: 15,
            dexterity: 15,
            intelligence: 12,
            faith: 80,
            arcane: 10,
        }),
        "Mt. Gelmir" => Some(RegionStatProfile {
            vigor: 50,
            mind: 12,
            endurance: 20,
            strength: 20,
            dexterity: 80,
            intelligence: 10,
            faith: 12,
            arcane: 10,
        }),
        "Mountaintops of the Giants" => Some(RegionStatProfile {
            vigor: 60,
            mind: 12,
            endurance: 20,
            strength: 80,
            dexterity: 20,
            intelligence: 10,
            faith: 12,
            arcane: 10,
        }),
        "ShadowRealm" => Some(RegionStatProfile {
            vigor: 60,
            mind: 18,
            endurance: 25,
            strength: 50,
            dexterity: 50,
            intelligence: 20,
            faith: 20,
            arcane: 20,
        }),
        _ => None,
    }
}

fn boss_region_for_flag<'a>(boss_regions: &'a BossRegions, flag_id: i32) -> Option<&'a str> {
    for group in boss_regions.iter() {
        if group.bosses.iter().any(|boss| boss.flag_id == flag_id) {
            return Some(group.region_name.as_str());
        }
    }
    None
}

fn active_region_name_for_phase<'a>(
    schedule: &'a RegionSchedule,
    phase_index: Option<usize>,
) -> Option<&'a str> {
    phase_index.map(|idx| schedule.phases[idx].region_name.as_str())
}

unsafe fn set_event_flag(evtflagman: *const u8, flag_id: i32, enabled: bool) -> bool {
    if evtflagman.is_null() {
        debug_log!(
            "[ignite_overlay] set_event_flag aborted: evtflagman null for flag_id={}, enabled={}",
            flag_id,
            enabled
        );
        return false;
    }

    write_flag(evtflagman, flag_id, enabled)
}

unsafe fn apply_action_set(
    evtflagman: *const u8,
    actions: Option<&PhaseActionSet>,
    enable_mode: bool,
) {
    let Some(actions) = actions else {
        return;
    };

    debug_log!(
        "[ignite_overlay] apply_action_set enable_mode={} on={:?} off={:?}",
        enable_mode,
        actions.set_flags_on,
        actions.set_flags_off
    );

    if enable_mode {
        for &flag_id in &actions.set_flags_on {
            let _ = set_event_flag(evtflagman, flag_id, true);
        }
        for &flag_id in &actions.set_flags_off {
            let _ = set_event_flag(evtflagman, flag_id, false);
        }
    } else {
        for &flag_id in &actions.set_flags_on {
            let _ = set_event_flag(evtflagman, flag_id, false);
        }
        for &flag_id in &actions.set_flags_off {
            let _ = set_event_flag(evtflagman, flag_id, true);
        }
    }
}

pub fn start_game_monitor(
    state: SharedState,
    igt: Arc<RwLock<u32>>,
    boss_regions: BossRegions,
    schedule: RegionSchedule,
    key_item_id: i32,
    poll_ms: u64,
    stop: Arc<AtomicBool>,
    save_dir: PathBuf,
    seed_id: String,
) {
    let flag_ids: Vec<i32> = boss_regions
        .iter()
        .flat_map(|region| region.bosses.iter().map(|boss| boss.flag_id))
        .collect();

    let great_rune_flags = vec![181, 182, 183, 184, 185, 186, 187];

    thread::spawn(move || unsafe {
        wait_for_system_init(&Program::current(), Duration::MAX)
            .expect("Timeout waiting for system init");

        let mut gamedataman: *const u8 = std::ptr::null();
        let mut evtflagman: *const u8 = std::ptr::null();

        if gamedataman.is_null() || evtflagman.is_null() {
            let mut delay = 250;
            loop {
                let game_opt = try_resolve_gamedataman();
                let evt_opt = try_resolve_eventflagman();
                if let (Some(evt), Some(gdm)) = (evt_opt, game_opt) {
                    gamedataman = gdm;
                    evtflagman = evt;
                    break;
                }
                if delay < 2000 {
                    delay *= 2;
                }
                thread::sleep(Duration::from_millis(delay));
            }
        }

        let mut entry_size = read_entry_size(evtflagman);
        while entry_size == 0 && !stop.load(Ordering::SeqCst) {
            debug_log!("[ignite_overlay] EventFlagMan not ready — waiting...");
            thread::sleep(Duration::from_millis(500));
            entry_size = read_entry_size(evtflagman);
        }

        debug_log!(
            "[ignite_overlay] Flag+IGT monitor started ({} boss flags, entry_size={})",
            flag_ids.len(),
            entry_size
        );

        let mut last_root = read_root(evtflagman);
        let mut boss_cache = build_cache(evtflagman, &flag_ids);
        let mut rune_cache = build_cache(evtflagman, &great_rune_flags);

        let update_interval = Duration::from_millis(poll_ms);

        while !stop.load(Ordering::SeqCst) {
            thread::sleep(update_interval);

            if evtflagman.is_null() || gamedataman.is_null() {
                debug_log!("[ignite_overlay] Managers lost — attempting reattach...");
                evtflagman = std::ptr::null();
                gamedataman = std::ptr::null();

                let mut delay = 250;
                while (evtflagman.is_null() || gamedataman.is_null())
                    && !stop.load(Ordering::SeqCst)
                {
                    if let (Some(evt), Some(gdm)) =
                        (try_resolve_eventflagman(), try_resolve_gamedataman())
                    {
                        evtflagman = evt;
                        gamedataman = gdm;
                        debug_log!("[ignite_overlay] Reattached managers successfully");
                        break;
                    }
                    if delay < 2000 {
                        delay *= 2;
                    }
                    thread::sleep(Duration::from_millis(delay));
                }

                continue;
            }

            let cur_root = read_root(evtflagman);
            if cur_root != last_root || boss_cache.is_empty() || rune_cache.is_empty() {
                let new_boss_cache = build_cache(evtflagman, &flag_ids);
                let new_rune_cache = build_cache(evtflagman, &great_rune_flags);
                let confirm_root = read_root(evtflagman);

                if confirm_root == cur_root {
                    boss_cache = new_boss_cache;
                    rune_cache = new_rune_cache;
                    last_root = cur_root;
                    debug_log!("[ignite_overlay] Rebuilt flag caches safely");
                } else {
                    debug_log!("[ignite_overlay] Flag tree changed mid-build; retrying next tick");
                    boss_cache.clear();
                    rune_cache.clear();
                }
                continue;
            }

            let mut current_igt = {
                let r = igt.read().unwrap();
                *r
            };

            if let Some(new_igt_raw) = read_in_game_time(gamedataman) {
                let new_igt = new_igt_raw / 1000;
                current_igt = new_igt;
                let mut w = igt.write().unwrap();
                *w = new_igt;

                debug_log!(
                    "[ignite_overlay] raw_igt={} stored_seconds={}",
                    new_igt_raw,
                    new_igt
                );
            }

            let was_initialized = {
                let reader = state.read().unwrap();
                reader.initialized
            };

            let now_initialized = current_igt > 0;

            if now_initialized && !was_initialized {
                debug_log!("[ignite_overlay] Run initialized at IGT {}", current_igt);
            }

            let (
                mut prev_flags,
                prev_qty,
                prev_death_count,
                prev_runes,
                prev_phase_index,
                prev_region_name,
                prev_counted_kills,
                prev_counted_total,
                mut boss_first_kill_time,
                mut fired_enter_once,
                mut counted_flags,
                mut cumulative_counted_kills,
            ) = {
                let reader = state.read().unwrap();
                (
                    reader.event_flags.clone(),
                    reader.key_item_quantity,
                    reader.death_count,
                    reader.great_runes,
                    if reader.initialized {
                        reader.active_phase_index
                    } else {
                        None
                    },
                    if reader.initialized {
                        reader.active_region_name.clone()
                    } else {
                        String::new()
                    },
                    if reader.initialized {
                        reader.counted_kills
                    } else {
                        0
                    },
                    if reader.initialized {
                        reader.counted_total
                    } else {
                        0
                    },
                    reader.boss_first_kill_time.clone(),
                    reader.fired_enter_once.clone(),
                    reader.counted_flags.clone(),
                    reader.cumulative_counted_kills,
                )
            };

            let mut changed = false;

            for (&flag_id, loc) in &boss_cache {
                if loc.base.is_null() {
                    continue;
                }

                let flag_state = read_from_flag_location(loc);
                match prev_flags.get(&flag_id) {
                    Some(&old) if old != flag_state => {
                        prev_flags.insert(flag_id, flag_state);

                        if !old && flag_state {
                            boss_first_kill_time.entry(flag_id).or_insert(current_igt);

                            let current_phase = if now_initialized {
                                active_phase_index(&schedule, current_igt)
                            } else {
                                None
                            };

                            let active_region =
                                active_region_name_for_phase(&schedule, current_phase);
                            let boss_region = boss_region_for_flag(&boss_regions, flag_id);

                            if let (Some(active_region), Some(boss_region)) =
                                (active_region, boss_region)
                            {
                                if boss_region.eq_ignore_ascii_case(active_region)
                                    && !counted_flags.contains(&flag_id)
                                {
                                    counted_flags.insert(flag_id);
                                    cumulative_counted_kills += 1;

                                    let persisted = PersistedRunState {
                                        seed: seed_id.clone(),
                                        counted_flags: counted_flags.clone(),
                                        cumulative_counted_kills,
                                    };

                                    let _ = save_run_state(&save_dir, &persisted);

                                    debug_log!(
                                        "[ignite_overlay] Counted boss kill: flag {} boss_region='{}' active_region='{}' cumulative={}",
                                        flag_id,
                                        boss_region,
                                        active_region,
                                        cumulative_counted_kills
                                    );
                                } else {
                                    debug_log!(
                                        "[ignite_overlay] Ignored boss kill: flag {} boss_region='{}' active_region='{}'",
                                        flag_id,
                                        boss_region,
                                        active_region
                                    );
                                }
                            } else {
                                debug_log!(
                                    "[ignite_overlay] Ignored boss kill: flag {} could not resolve active region or boss region",
                                    flag_id
                                );
                            }

                            debug_log!(
                                "[ignite_overlay] Boss kill detected: flag {} at IGT {}",
                                flag_id,
                                current_igt
                            );
                        }

                        changed = true;
                    }
                    None => {
                        prev_flags.insert(flag_id, flag_state);

                        if flag_state {
                            boss_first_kill_time.entry(flag_id).or_insert(current_igt);
                        }

                        changed = true;
                    }
                    _ => {}
                }
            }

            let mut new_runes = 0;
            for (_, loc) in &rune_cache {
                if loc.base.is_null() {
                    continue;
                }
                if read_from_flag_location(loc) {
                    new_runes += 1;
                }
            }

            if new_runes != prev_runes {
                debug_log!(
                    "[ignite_overlay] Great Rune count updated: {} -> {}",
                    prev_runes,
                    new_runes
                );
                changed = true;
            }

            let new_qty = get_key_item_quantity(key_item_id);
            if new_qty != prev_qty {
                debug_log!(
                    "[ignite_overlay] New shard count: {}, previous shard count: {}",
                    new_qty,
                    prev_qty
                );
                changed = true;
            }

            let mut new_death_count = prev_death_count;
            if let Some(death_count) = read_death_count(gamedataman) {
                if prev_death_count != death_count {
                    new_death_count = death_count;
                    changed = true;
                }
            }

            let new_phase_index = if now_initialized {
                active_phase_index(&schedule, current_igt)
            } else {
                None
            };

            let mut new_region_name = String::new();
            let mut new_counted_total = 0u32;
            let new_counted_kills = cumulative_counted_kills;

            if let Some(phase_idx) = new_phase_index {
                let phase = &schedule.phases[phase_idx];
                new_region_name = phase.region_name.clone();

                if let Some(group) = boss_regions
                    .iter()
                    .find(|g| g.region_name.eq_ignore_ascii_case(&phase.region_name))
                {
                    new_counted_total = group.bosses.len() as u32;
                }
            }

            let region_changed = now_initialized
                && (new_phase_index != prev_phase_index || new_region_name != prev_region_name);

            let first_init_apply =
                now_initialized && !was_initialized && !new_region_name.is_empty();

            if now_initialized != was_initialized
                || new_phase_index != prev_phase_index
                || new_region_name != prev_region_name
                || new_counted_kills != prev_counted_kills
                || new_counted_total != prev_counted_total
            {
                changed = true;
            }

            if new_phase_index != prev_phase_index {
                debug_log!(
                    "[ignite_overlay] Phase change: {:?} -> {:?}",
                    prev_phase_index,
                    new_phase_index
                );

                if let Some(old_idx) = prev_phase_index {
                    let old_phase = &schedule.phases[old_idx];
                    apply_action_set(evtflagman, old_phase.on_exit.as_ref(), true);
                    apply_action_set(evtflagman, old_phase.while_active.as_ref(), false);
                }

                if let Some(new_idx) = new_phase_index {
                    let new_phase = &schedule.phases[new_idx];

                    if !fired_enter_once.contains(&new_idx) {
                        apply_action_set(evtflagman, new_phase.on_enter_once.as_ref(), true);
                        fired_enter_once.insert(new_idx);
                    }

                    apply_action_set(evtflagman, new_phase.while_active.as_ref(), true);
                }
            }

            if region_changed || first_init_apply {
                if let Some(profile) = region_stat_profile(&new_region_name) {
                    match try_apply_region_stats(profile) {
                        Ok(()) => {
                            debug_log!(
                                "[ignite_overlay] Applied region stat profile for '{}'",
                                new_region_name
                            );
                        }
                        Err(err) => {
                            debug_log!(
                                "[ignite_overlay] Failed to apply region stat profile for '{}': {}",
                                new_region_name,
                                err
                            );
                        }
                    }
                } else if !new_region_name.is_empty() {
                    debug_log!(
                        "[ignite_overlay] No region stat profile configured for '{}'",
                        new_region_name
                    );
                }
            }

            debug_log!(
                "[ignite_overlay] phase={:?} region='{}' kills={}/{} initialized={}",
                new_phase_index,
                new_region_name,
                new_counted_kills,
                new_counted_total,
                now_initialized
            );

            if changed {
                debug_log!("[ignite_overlay] Change detected during monitoring. Updating app state.");
                let mut w = state.write().unwrap();
                w.event_flags = prev_flags;
                w.key_item_quantity = new_qty;
                w.death_count = new_death_count;
                w.great_runes = new_runes;

                w.initialized = now_initialized;
                w.active_phase_index = new_phase_index;
                w.active_region_name = new_region_name;
                w.counted_kills = new_counted_kills;
                w.counted_total = new_counted_total;

                w.counted_flags = counted_flags;
                w.cumulative_counted_kills = cumulative_counted_kills;

                w.boss_first_kill_time = boss_first_kill_time;
                w.fired_enter_once = fired_enter_once;
            }
        }

        debug_log!("[ignite_overlay] Monitor thread exiting gracefully");
    });
}