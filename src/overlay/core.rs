use std::{
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    debug_log,
    er::{
        events::{
            build_cache, read_entry_size, read_from_flag_location, read_root,
            try_resolve_eventflagman, write_flag,
        },
        gamedata::{read_death_count, read_in_game_time, try_resolve_gamedataman},
        inventory::get_key_item_quantity,
    },
    overlay::data::{EventFlagRule, EventFlagSchedule, SharedState},
};

#[derive(Clone, Default)]
struct EventFlagRuleState {
    last_apply_tick: u32,
    pending_remove_at: Option<u32>,
    active: bool,
    active_set_flags_on: Vec<i32>,
    active_set_flags_off: Vec<i32>,
    previous_flag_states: Vec<(i32, bool)>,
}

struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new() -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9_7f4a_7c15);

        Self {
            state: seed ^ 0xa076_1d64_78bd_642f,
        }
    }

    fn next_u32(&mut self) -> u32 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        (self.state >> 32) as u32
    }

    fn range_inclusive(&mut self, min: usize, max: usize) -> usize {
        if max <= min {
            return min;
        }

        min + (self.next_u32() as usize % (max - min + 1))
    }
}

fn reset_event_flag_rule_states(states: &mut [EventFlagRuleState]) {
    for state in states {
        state.last_apply_tick = 0;
        state.pending_remove_at = None;
        state.active = false;
        state.active_set_flags_on.clear();
        state.active_set_flags_off.clear();
        state.previous_flag_states.clear();
        state.previous_flag_states.clear();
    }
}

fn current_rule_tick(rule: &EventFlagRule, current_igt_seconds: u32) -> u32 {
    let interval_seconds = rule.interval_minutes.saturating_mul(60);
    if interval_seconds == 0 {
        return 0;
    }

    let start_after = rule.start_after_seconds.unwrap_or(0);
    if current_igt_seconds < start_after.saturating_add(interval_seconds) {
        return 0;
    }

    current_igt_seconds.saturating_sub(start_after) / interval_seconds
}

fn seed_event_flag_rule_states(
    rules: &[EventFlagRule],
    states: &mut [EventFlagRuleState],
    current_igt_seconds: u32,
) {
    for (rule, state) in rules.iter().zip(states.iter_mut()) {
        state.last_apply_tick = current_rule_tick(rule, current_igt_seconds);
        state.pending_remove_at = None;
        state.active = false;
        state.active_set_flags_on.clear();
        state.active_set_flags_off.clear();
        state.previous_flag_states.clear();
    }
}

fn current_rule_interval_start(rule: &EventFlagRule, current_igt_seconds: u32) -> Option<u32> {
    let tick = current_rule_tick(rule, current_igt_seconds);
    if tick == 0 {
        return None;
    }

    let start_after = rule.start_after_seconds.unwrap_or(0);
    let interval_seconds = rule.interval_minutes.saturating_mul(60);
    Some(start_after.saturating_add(tick.saturating_mul(interval_seconds)))
}

unsafe fn read_event_flag(evtflagman: *const u8, flag_id: i32) -> Option<bool> {
    let cache = unsafe { build_cache(evtflagman, &vec![flag_id]) };
    cache
        .get(&flag_id)
        .map(|loc| unsafe { read_from_flag_location(loc) })
}

unsafe fn restore_active_rule_from_current_flags(
    evtflagman: *const u8,
    rule: &EventFlagRule,
    state: &mut EventFlagRuleState,
    current_igt_seconds: u32,
) {
    let Some(remove_after_seconds) = rule.remove_after_seconds.filter(|seconds| *seconds > 0)
    else {
        return;
    };

    let Some(interval_start) = current_rule_interval_start(rule, current_igt_seconds) else {
        return;
    };

    let remove_at = interval_start.saturating_add(remove_after_seconds);
    if current_igt_seconds >= remove_at {
        return;
    }

    let active_set_flags_on = rule
        .set_flags_on
        .iter()
        .copied()
        .filter(|&flag_id| unsafe { read_event_flag(evtflagman, flag_id) }.unwrap_or(false))
        .collect::<Vec<_>>();

    let active_set_flags_off = rule
        .set_flags_off
        .iter()
        .copied()
        .filter(|&flag_id| !unsafe { read_event_flag(evtflagman, flag_id) }.unwrap_or(true))
        .collect::<Vec<_>>();

    if active_set_flags_on.is_empty() && active_set_flags_off.is_empty() {
        return;
    }

    state.active = true;
    state.pending_remove_at = Some(remove_at);
    state.previous_flag_states = active_set_flags_on
        .iter()
        .map(|&flag_id| (flag_id, false))
        .chain(active_set_flags_off.iter().map(|&flag_id| (flag_id, true)))
        .collect();
    state.active_set_flags_on = active_set_flags_on;
    state.active_set_flags_off = active_set_flags_off;

    debug_log!(
        "[ignite_overlay] Restored active event flag rule '{}' from current flags at IGT {} remove_at {}",
        rule.name.as_deref().unwrap_or("unnamed"),
        current_igt_seconds,
        remove_at
    );
}

fn flag_list_text(flags: &[i32]) -> String {
    flags
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn labeled_flag_list_text(rule: &EventFlagRule, flags: &[i32]) -> String {
    flags
        .iter()
        .map(|id| {
            rule.flag_labels
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.to_string())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn rule_display_name(rule: &EventFlagRule, state: &EventFlagRuleState) -> String {
    let mut labels = Vec::new();

    if !state.active_set_flags_on.is_empty() {
        labels.push(labeled_flag_list_text(rule, &state.active_set_flags_on));
    }
    if !state.active_set_flags_off.is_empty() {
        labels.push(labeled_flag_list_text(rule, &state.active_set_flags_off));
    }

    labels
        .into_iter()
        .filter(|label| !label.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

fn select_random_flags(flags: &[i32], rule: &EventFlagRule, rng: &mut SimpleRng) -> Vec<i32> {
    if flags.is_empty() {
        return Vec::new();
    }

    let min = rule.random_min_flags.unwrap_or(1).clamp(0, flags.len());
    let max = rule
        .random_max_flags
        .unwrap_or(flags.len())
        .clamp(min, flags.len());
    let count = rng.range_inclusive(min, max);

    let mut shuffled = flags.to_vec();
    for idx in (1..shuffled.len()).rev() {
        let swap_idx = rng.range_inclusive(0, idx);
        shuffled.swap(idx, swap_idx);
    }

    shuffled.truncate(count);
    shuffled
}

fn selected_rule_flags(rule: &EventFlagRule, rng: &mut SimpleRng) -> (Vec<i32>, Vec<i32>) {
    if !rule.randomize_flags {
        return (rule.set_flags_on.clone(), rule.set_flags_off.clone());
    }

    (
        select_random_flags(&rule.set_flags_on, rule, rng),
        select_random_flags(&rule.set_flags_off, rule, rng),
    )
}

unsafe fn apply_event_flags(
    evtflagman: *const u8,
    set_flags_on: &[i32],
    set_flags_off: &[i32],
    active: bool,
) {
    for &flag_id in set_flags_on {
        let _ = unsafe { write_flag(evtflagman, flag_id, active) };
    }

    for &flag_id in set_flags_off {
        let _ = unsafe { write_flag(evtflagman, flag_id, !active) };
    }
}

unsafe fn capture_flag_states(evtflagman: *const u8, flags: &[i32]) -> Vec<(i32, bool)> {
    let mut unique_flags = Vec::new();
    for &flag_id in flags {
        if !unique_flags.contains(&flag_id) {
            unique_flags.push(flag_id);
        }
    }

    let cache = unsafe { build_cache(evtflagman, &unique_flags) };
    unique_flags
        .into_iter()
        .filter_map(|flag_id| {
            cache
                .get(&flag_id)
                .map(|loc| (flag_id, unsafe { read_from_flag_location(loc) }))
        })
        .collect()
}

unsafe fn restore_flag_states(evtflagman: *const u8, previous_states: &[(i32, bool)]) {
    for &(flag_id, was_enabled) in previous_states {
        let _ = unsafe { write_flag(evtflagman, flag_id, was_enabled) };
    }
}

unsafe fn apply_always_on_flags(evtflagman: *const u8, flags: &[i32]) {
    for &flag_id in flags {
        let _ = unsafe { write_flag(evtflagman, flag_id, true) };
    }
}

fn active_event_flag_text(
    always_on_flags: &[i32],
    always_on_applied: bool,
    rules: &[EventFlagRule],
    states: &[EventFlagRuleState],
) -> String {
    let mut names = Vec::new();

    if always_on_applied && !always_on_flags.is_empty() {
        names.push(format!("Always On: {}", flag_list_text(always_on_flags)));
    }

    names.extend(
        rules
            .iter()
            .zip(states.iter())
            .filter(|(_, state)| state.active)
            .map(|(rule, state)| rule_display_name(rule, state))
            .filter(|name| !name.is_empty()),
    );

    names.join(", ")
}

unsafe fn update_event_flag_rules(
    evtflagman: *const u8,
    rules: &[EventFlagRule],
    states: &mut [EventFlagRuleState],
    current_igt_seconds: u32,
    always_on_flags: &[i32],
    always_on_applied: bool,
    rng: &mut SimpleRng,
) -> String {
    for (rule, state) in rules.iter().zip(states.iter_mut()) {
        if let Some(remove_at) = state.pending_remove_at {
            if current_igt_seconds >= remove_at {
                unsafe {
                    apply_event_flags(
                        evtflagman,
                        &state.active_set_flags_on,
                        &state.active_set_flags_off,
                        false,
                    )
                };
                state.pending_remove_at = None;
                state.active = false;
                state.active_set_flags_on.clear();
                state.active_set_flags_off.clear();
                state.previous_flag_states.clear();
                state.previous_flag_states.clear();

                debug_log!(
                    "[ignite_overlay] Removed event flag rule '{}' at IGT {}",
                    rule.name.as_deref().unwrap_or("unnamed"),
                    current_igt_seconds
                );
            }
        }

        if rule.interval_minutes == 0
            || (rule.set_flags_on.is_empty() && rule.set_flags_off.is_empty())
        {
            continue;
        }

        let tick = current_rule_tick(rule, current_igt_seconds);
        if tick <= state.last_apply_tick {
            continue;
        }

        let (set_flags_on, set_flags_off) = selected_rule_flags(rule, rng);
        if set_flags_on.is_empty() && set_flags_off.is_empty() {
            state.last_apply_tick = tick;
            continue;
        }

        if state.active {
            unsafe { restore_flag_states(evtflagman, &state.previous_flag_states) };
            state.pending_remove_at = None;
            state.active = false;
            state.active_set_flags_on.clear();
            state.active_set_flags_off.clear();
            state.previous_flag_states.clear();
        }

        let mut touched_flags = set_flags_on.clone();
        touched_flags.extend(set_flags_off.iter().copied());
        state.previous_flag_states = unsafe { capture_flag_states(evtflagman, &touched_flags) };

        unsafe { apply_event_flags(evtflagman, &set_flags_on, &set_flags_off, true) };
        state.last_apply_tick = tick;
        state.active = true;
        state.active_set_flags_on = set_flags_on;
        state.active_set_flags_off = set_flags_off;

        if let Some(remove_after_seconds) = rule.remove_after_seconds.filter(|seconds| *seconds > 0)
        {
            state.pending_remove_at =
                Some(current_igt_seconds.saturating_add(remove_after_seconds));
        }

        debug_log!(
            "[ignite_overlay] Applied event flag rule '{}' at IGT {} tick {}",
            rule.name.as_deref().unwrap_or("unnamed"),
            current_igt_seconds,
            tick
        );
    }

    active_event_flag_text(always_on_flags, always_on_applied, rules, states)
}

pub fn start_game_monitor(
    state: SharedState,
    igt: Arc<RwLock<u32>>,
    flag_ids: Vec<i32>,
    event_flags_enabled: bool,
    event_flag_schedule: Option<EventFlagSchedule>,
    key_item_id: i32,
    poll_ms: u64,
    stop: Arc<AtomicBool>,
) {
    let great_rune_flags = vec![181, 182, 183, 184, 185, 186, 187];

    thread::spawn(move || unsafe {
        debug_log!("[ignite_overlay] Monitor thread started; resolving managers...");

        let mut gamedataman: *const u8 = std::ptr::null();
        let mut evtflagman: *const u8 = std::ptr::null();

        if gamedataman.is_null() || evtflagman.is_null() {
            let mut delay = 250;
            let mut attempts = 0u32;
            loop {
                let game_opt = try_resolve_gamedataman();
                let evt_opt = try_resolve_eventflagman();
                if let (Some(evt), Some(gdm)) = (evt_opt, game_opt) {
                    gamedataman = gdm;
                    evtflagman = evt;
                    debug_log!(
                        "[ignite_overlay] Resolved managers: GameDataMan=0x{:x}, EventFlagMan=0x{:x}",
                        gamedataman as usize,
                        evtflagman as usize
                    );
                    break;
                }

                attempts += 1;
                if attempts == 1 || attempts % 10 == 0 {
                    debug_log!(
                        "[ignite_overlay] Waiting for managers: GameDataMan={} EventFlagMan={} attempts={}",
                        game_opt.is_some(),
                        evt_opt.is_some(),
                        attempts
                    );
                }

                if delay < 2000 {
                    delay *= 2;
                }
                thread::sleep(Duration::from_millis(delay));
            }
        }

        let mut entry_size = read_entry_size(evtflagman);
        while entry_size == 0 && !stop.load(Ordering::SeqCst) {
            debug_log!("[ignite_overlay] EventFlagMan not ready - waiting...");
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
        let event_flag_schedule = event_flag_schedule.unwrap_or_default();
        let always_on_flags = event_flag_schedule.always_on_flags;
        let event_flag_rules = event_flag_schedule.interval_rules;
        let mut event_flag_rule_states =
            vec![EventFlagRuleState::default(); event_flag_rules.len()];
        let mut last_event_igt_seconds = 0u32;
        let mut event_flag_rules_seeded = false;
        let mut always_on_flags_applied = false;
        let mut rng = SimpleRng::new();

        while !stop.load(Ordering::SeqCst) {
            thread::sleep(update_interval);

            if evtflagman.is_null() || gamedataman.is_null() {
                debug_log!("[ignite_overlay] Managers lost - attempting reattach...");
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

            let (mut prev_flags, prev_qty, prev_death_count, prev_runes, prev_events) = {
                let reader = state.read().unwrap();
                (
                    reader.event_flags.clone(),
                    reader.key_item_quantity,
                    reader.death_count,
                    reader.great_runes,
                    reader.current_events.clone(),
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
                        changed = true;
                    }
                    None => {
                        prev_flags.insert(flag_id, flag_state);
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

            let mut current_igt_seconds = last_event_igt_seconds;
            if let Some(new_igt_ms) = read_in_game_time(gamedataman) {
                current_igt_seconds = new_igt_ms / 1000;
            }

            if current_igt_seconds < last_event_igt_seconds {
                reset_event_flag_rule_states(&mut event_flag_rule_states);
                event_flag_rules_seeded = false;
                always_on_flags_applied = false;
            }

            if event_flags_enabled && current_igt_seconds > 0 && !event_flag_rules_seeded {
                seed_event_flag_rule_states(
                    &event_flag_rules,
                    &mut event_flag_rule_states,
                    current_igt_seconds,
                );
                for (rule, state) in event_flag_rules
                    .iter()
                    .zip(event_flag_rule_states.iter_mut())
                {
                    restore_active_rule_from_current_flags(
                        evtflagman,
                        rule,
                        state,
                        current_igt_seconds,
                    );
                }

                event_flag_rules_seeded = true;
                debug_log!(
                    "[ignite_overlay] Seeded event flag intervals at IGT {}",
                    current_igt_seconds
                );
            }

            last_event_igt_seconds = current_igt_seconds;

            let mut new_current_events = prev_events.clone();
            if event_flags_enabled && current_igt_seconds > 0 {
                if !always_on_flags_applied && !always_on_flags.is_empty() {
                    apply_always_on_flags(evtflagman, &always_on_flags);
                    always_on_flags_applied = true;
                    debug_log!(
                        "[ignite_overlay] Applied always-on event flags: {}",
                        flag_list_text(&always_on_flags)
                    );
                }

                new_current_events = update_event_flag_rules(
                    evtflagman,
                    &event_flag_rules,
                    &mut event_flag_rule_states,
                    current_igt_seconds,
                    &always_on_flags,
                    always_on_flags_applied,
                    &mut rng,
                );
            }

            if new_current_events != prev_events {
                changed = true;
            }

            if changed {
                debug_log!(
                    "[ignite_overlay] Change detected during game monitoring. Updating app state."
                );
                let mut w = state.write().unwrap();
                w.event_flags = prev_flags;
                w.key_item_quantity = new_qty;
                w.death_count = new_death_count;
                w.great_runes = new_runes;
                w.current_events = new_current_events;
            }

            if let Some(new_igt_ms) = read_in_game_time(gamedataman) {
                let mut w = igt.write().unwrap();
                *w = new_igt_ms;
            }
        }

        debug_log!("[ignite_overlay] Monitor thread exiting gracefully");
    });
}
