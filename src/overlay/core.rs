use std::{thread, time::{Duration}, sync::{Arc, RwLock, atomic::{AtomicBool, Ordering}}};
use eldenring_util::system::wait_for_system_init;
use fromsoftware_shared::{program::Program};
use crate::{
    debug_log,
    overlay::data::SharedState,
    er::{
        events::{try_resolve_eventflagman, read_entry_size, read_root, build_cache, read_from_flag_location},
        inventory::get_key_item_quantity,
        gamedata::{try_resolve_gamedataman, read_death_count, read_in_game_time}

    },
};

pub fn start_game_monitor(
    state: SharedState,
    igt: Arc<RwLock<u32>>,
    flag_ids: Vec<i32>,
    key_item_id: i32,
    poll_ms: u64,
    stop: Arc<AtomicBool>
) {
    // The event flags that indicate each Great Rune being owned.
    let great_rune_flags = vec![181, 182, 183, 184, 185, 186, 187];

    thread::spawn(move || unsafe {
        // Wait for core systems
        wait_for_system_init(&Program::current(), Duration::MAX)
            .expect("Timeout waiting for system init");

        // --- Initialization ---
        let mut gamedataman: *const u8 = std::ptr::null();
        let mut evtflagman: *const u8 = std::ptr::null();
        if gamedataman.is_null() && evtflagman.is_null() {
            let mut delay = 250;
            loop {
                let game_opt = try_resolve_gamedataman();
                let evt_opt = try_resolve_eventflagman();
                if let (Some(evt), Some(gdm)) = (evt_opt, game_opt) {
                    gamedataman = gdm;
                    evtflagman = evt;
                    break;
                }
                if delay < 2000 { delay *= 2 };
                thread::sleep(Duration::from_millis(delay));
            }
        }
        let mut entry_size = read_entry_size(evtflagman);
        while entry_size == 0 && !stop.load(Ordering::SeqCst){
            debug_log!("[ignite_overlay] EventFlagMan not ready — waiting...");
            thread::sleep(Duration::from_millis(500));
            entry_size = read_entry_size(evtflagman);
        }

        debug_log!(
            "[ignite_overlay] Flag+IGT monitor started ({} flags, entry_size={})",
            flag_ids.len(),
            entry_size
        );

        let mut last_root = read_root(evtflagman);
        let mut boss_cache = build_cache(evtflagman, &flag_ids);
        let mut rune_cache = build_cache(evtflagman, &great_rune_flags);

        let update_interval = Duration::from_millis(poll_ms);

        while !stop.load(Ordering::SeqCst) {
            // update interval
            thread::sleep(update_interval);

            // Check that event flag manager and game data manager pointers are valid. Reattach if not.
            if evtflagman.is_null() || gamedataman.is_null() {
                debug_log!("[ignite_overlay] Managers lost — attempting reattach...");
                evtflagman = std::ptr::null();
                gamedataman = std::ptr::null();

                let mut delay = 250;
                while (evtflagman.is_null() || gamedataman.is_null()) && !stop.load(Ordering::SeqCst) {
                    if let (Some(evt), Some(gdm)) = (try_resolve_eventflagman(), try_resolve_gamedataman()) {
                        evtflagman = evt;
                        gamedataman = gdm;
                        debug_log!("[ignite_overlay] Reattached managers successfully");
                        break;
                    }
                    if delay < 2000 { delay *= 2; }
                    thread::sleep(Duration::from_millis(delay));
                }

                continue;
            }

            // Initialize or rebuild flag tree. Wait a tick after building cache for stable flag tree.
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

            // --- Step 1: snapshot previous state
            let (mut prev_flags, prev_qty, prev_death_count, prev_runes) = {
                let reader = state.read().unwrap();
                (
                    reader.event_flags.clone(),
                    reader.key_item_quantity,
                    reader.death_count,
                    reader.great_runes,
                )
            };

            // --- Step 2: read new data
            let mut changed = false;

            // Update event flags
            for (&flag_id, loc) in &boss_cache {
                if loc.base.is_null() {
                    continue;
                }

                let flag_state = read_from_flag_location(loc);
                match prev_flags.get(&flag_id) {
                    Some(&old) if old != flag_state => {
                        prev_flags.insert(flag_id, flag_state);
                        changed = true;
                    }
                    None => {
                        prev_flags.insert(flag_id, flag_state);
                        changed = true;
                    }
                    _ => {}
                }
            };

            // Update Great Rune count
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
                debug_log!("[ignite_overlay] Great Rune count updated: {} -> {}", prev_runes, new_runes);
                changed = true;
            }

            // Query the player's inventory for the key item id
            let new_qty = get_key_item_quantity(key_item_id);
            if new_qty != prev_qty {
                debug_log!("New shard count: {}, previous shard count: {}", new_qty, prev_qty);
                changed = true;
            }

            // Check player's death count
            let mut new_death_count = prev_death_count;
            if let Some(death_count) = read_death_count(gamedataman) {
                if prev_death_count != death_count {
                    new_death_count = death_count;
                    changed = true;
                }
            }

            // --- Step 3: commit if changed
            if changed {
                debug_log!("Change detected during game monitoring. Updating app state.");
                let mut w = state.write().unwrap();
                w.event_flags = prev_flags;
                w.key_item_quantity = new_qty;
                w.death_count = new_death_count;
                w.great_runes = new_runes;
            }

            // Update game time separately from the rest of app state, since it changes predictably
            if let Some(new_igt) = read_in_game_time(gamedataman) {
                let mut w = igt.write().unwrap();
                *w = new_igt;
            };
        }
        debug_log!("[ignite_overlay] Monitor thread exiting gracefully");
    });
}
