use std::{
    collections::HashMap,
    fmt::Write,
    sync::{
        atomic::AtomicBool,
        Arc, RwLock,
    },
    time::{Duration, Instant},
};

use hudhook::ImguiRenderLoop;
use imgui::{Key, Ui};

use crate::{
    debug_log,
    er::grace::try_auto_activate_nearby_grace,
    overlay::{
        core::start_game_monitor,
        data::{
            create_state, load_localized_boss_data, load_region_schedule, load_run_state, AppState,
            BossRegions, RegionSchedule,
        },
        style::{
            apply_common_config, apply_style_config, parse_key_combo, read_config, IgniteConfig,
            TimerMode, DEFAULT_DISPLAY_TEXT, DEFAULT_PANEL_POS,
        },
    },
    util::{
        debug::attach_console,
        introspection::get_dll_directory,
        text_formatter::format_display_text,
    },
};

pub struct EROverlayUi {
    last_click_time: Instant,
    last_toggle_time: Instant,
    timer_buf: String,
    full_mode: bool,

    config: Option<IgniteConfig>,
    config_error: Option<String>,

    toggle_full_mode_keys: Option<Vec<Key>>,
    click_action_keys: Option<Vec<imgui::Key>>,

    state: Arc<RwLock<AppState>>,
    igt: Arc<RwLock<u32>>,
    boss_regions: Option<BossRegions>,
    region_schedule: Option<RegionSchedule>,

    timer_mode: TimerMode,
    prep_time_ms: u32,
    timer_target_ms: u32,

    monitor_stop: Arc<AtomicBool>,

    dll_dir: Option<std::path::PathBuf>,
    seed_id: String,
}

impl EROverlayUi {
    pub fn new() -> Self {
        let (config, config_error) = match read_config() {
            Ok(cfg) => (Some(cfg), None),
            Err(err) => (None, Some(err)),
        };

        let toggle_full_mode_keys = config
            .as_ref()
            .and_then(|c| c.input.as_ref())
            .and_then(|i| i.toggle_full_mode.clone())
            .map(|combo| parse_key_combo(&combo));

        let click_action_keys = config
            .as_ref()
            .and_then(|c| c.input.as_ref())
            .and_then(|i| i.click_action.clone())
            .map(|s| parse_key_combo(&s));

        let language = config
            .as_ref()
            .and_then(|c| c.common.as_ref())
            .and_then(|cc| cc.language.as_ref())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or("engus");

        let data_file = config
            .as_ref()
            .and_then(|c| c.boss.as_ref())
            .and_then(|cc| cc.data_file.as_ref())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or("bosses.json");

        let schedule_file = config
            .as_ref()
            .and_then(|c| c.boss.as_ref())
            .and_then(|cc| cc.schedule_file.as_ref())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or("region_schedule.json");

        let seed_id = config
            .as_ref()
            .and_then(|c| c.boss.as_ref())
            .and_then(|cc| cc.seed.as_ref())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "default_seed".to_string());

        let dll_dir = get_dll_directory();

        let boss_regions = dll_dir
            .as_ref()
            .and_then(|dir| load_localized_boss_data(dir, language, data_file));

        let region_schedule = dll_dir
            .as_ref()
            .and_then(|dir| load_region_schedule(dir, schedule_file));

        let timer_mode = config
            .as_ref()
            .and_then(|c| c.timer.as_ref())
            .map(|t| t.mode)
            .unwrap_or(TimerMode::Regular);

        let prep_time_ms = config
            .as_ref()
            .and_then(|c| c.timer.as_ref())
            .and_then(|t| t.prep_minutes)
            .unwrap_or(0)
            * 60_000;

        let timer_target_ms = config
            .as_ref()
            .and_then(|c| c.timer.as_ref())
            .and_then(|t| t.timer_minutes)
            .unwrap_or(0)
            * 60_000;

        Self {
            last_click_time: Instant::now(),
            last_toggle_time: Instant::now(),
            timer_buf: String::with_capacity(32),
            full_mode: false,
            config,
            config_error,
            toggle_full_mode_keys,
            click_action_keys,
            state: create_state(),
            igt: Arc::new(RwLock::new(0)),
            boss_regions,
            region_schedule,
            timer_mode,
            prep_time_ms,
            timer_target_ms,
            monitor_stop: Arc::new(AtomicBool::new(false)),
            dll_dir,
            seed_id,
        }
    }

    fn route_region_names_in_order(&self) -> Vec<String> {
        let Some(schedule) = self.region_schedule.as_ref() else {
            return Vec::new();
        };

        let mut names = Vec::new();
        for phase in &schedule.phases {
            if !names.contains(&phase.region_name) {
                names.push(phase.region_name.clone());
            }
        }
        names
    }

    fn build_route_string(&self, current_region: &str) -> String {
        self.route_region_names_in_order()
            .into_iter()
            .map(|region_name| {
                if !current_region.is_empty() && region_name == current_region {
                    format!("[{}]", region_name)
                } else {
                    region_name
                }
            })
            .collect::<Vec<_>>()
            .join(" -> ")
    }

    fn build_route_totals(&self) -> (usize, usize) {
        let route_regions = self.route_region_names_in_order();

        let mut total_possible = 0usize;
        let mut total_killed = 0usize;

        let Some(all_regions) = self.boss_regions.as_ref() else {
            return (0, 0);
        };

        let Ok(state) = self.state.read() else {
            return (0, 0);
        };

        for region_name in route_regions {
            if let Some(region) = all_regions.iter().find(|r| r.region_name == region_name) {
                total_possible += region.bosses.len();
                total_killed += region
                    .bosses
                    .iter()
                    .filter(|b| state.counted_flags.contains(&b.flag_id))
                    .count();
            }
        }

        (total_killed, total_possible)
    }

    fn build_current_region_totals(&self, current_region: &str) -> (usize, usize) {
        if current_region.is_empty() {
            return (0, 0);
        }

        let Some(all_regions) = self.boss_regions.as_ref() else {
            return (0, 0);
        };

        let Ok(state) = self.state.read() else {
            return (0, 0);
        };

        if let Some(region) = all_regions
            .iter()
            .find(|r| r.region_name.eq_ignore_ascii_case(current_region))
        {
            let total_possible = region.bosses.len();
            let total_killed = region
                .bosses
                .iter()
                .filter(|b| state.counted_flags.contains(&b.flag_id))
                .count();

            (total_killed, total_possible)
        } else {
            (0, 0)
        }
    }

    fn render_closed(&mut self, ui: &Ui) {
        self.write_igt();

        let mut death_count: u32 = 0;
        let mut shard_count: u32 = 0;
        let mut great_runes_count = 0;
        let mut region_name = String::new();
        let mut phase_name = String::new();

        if let Ok(state) = self.state.read() {
            shard_count = state.key_item_quantity;
            death_count = state.death_count;
            great_runes_count = state.great_runes;
            region_name = state.active_region_name.clone();

            if let (Some(schedule), Some(phase_idx)) =
                (self.region_schedule.as_ref(), state.active_phase_index)
            {
                if let Some(phase) = schedule.phases.get(phase_idx) {
                    phase_name = phase.name.clone();
                }
            }
        }

        let (defeated, total) = self.build_current_region_totals(&region_name);
        let route_regions = self.build_route_string(&region_name);
        let (route_kills, route_total) = self.build_route_totals();

        let vars = {
            let igt_str = self.timer_buf.clone();
            HashMap::from([
                ("kills", defeated.to_string()),
                ("total", total.to_string()),
                ("counted_kills", defeated.to_string()),
                ("counted_total", total.to_string()),
                ("deaths", death_count.to_string()),
                ("igt", igt_str),
                ("shards", shard_count.to_string()),
                ("runes", great_runes_count.to_string()),
                ("region", region_name),
                ("phase", phase_name),
                ("route", route_regions.clone()),
                ("route_names", route_regions),
                ("route_total", route_total.to_string()),
                ("route_kills", route_kills.to_string()),
            ])
        };

        let template = self
            .config
            .as_ref()
            .and_then(|c| c.overlay.as_ref())
            .and_then(|o| o.display_text.as_deref())
            .unwrap_or(DEFAULT_DISPLAY_TEXT);

        let lines = format_display_text(template, &vars);
        Self::render_centered_text_block(ui, &lines);

        let total_h = ui.text_line_height_with_spacing() * lines.len() as f32 + 8.0;

        if Self::is_click_in_header(ui, total_h) {
            let now = Instant::now();
            if now.duration_since(self.last_toggle_time) > Duration::from_millis(300) {
                self.full_mode = true;
                self.last_toggle_time = now;
                debug_log!("[ignite_overlay] Clicked compact overlay — expanding");
            }
        }
    }

    fn render_open(&mut self, ui: &Ui) {
        self.write_igt();

        let mut death_count: u32 = 0;
        let mut shard_count: u32 = 0;
        let mut great_runes_count = 0;
        let mut region_name = String::new();
        let mut phase_name = String::new();

        if let Ok(state) = self.state.read() {
            shard_count = state.key_item_quantity;
            death_count = state.death_count;
            great_runes_count = state.great_runes;
            region_name = state.active_region_name.clone();

            if let (Some(schedule), Some(phase_idx)) =
                (self.region_schedule.as_ref(), state.active_phase_index)
            {
                if let Some(phase) = schedule.phases.get(phase_idx) {
                    phase_name = phase.name.clone();
                }
            }
        }

        let (defeated, total) = self.build_current_region_totals(&region_name);
        let route_regions = self.build_route_string(&region_name);
        let (route_kills, route_total) = self.build_route_totals();

        let vars = {
            let igt_str = self.timer_buf.clone();
            HashMap::from([
                ("kills", defeated.to_string()),
                ("total", total.to_string()),
                ("counted_kills", defeated.to_string()),
                ("counted_total", total.to_string()),
                ("deaths", death_count.to_string()),
                ("igt", igt_str),
                ("shards", shard_count.to_string()),
                ("runes", great_runes_count.to_string()),
                ("region", region_name.clone()),
                ("phase", phase_name.clone()),
                ("route", route_regions.clone()),
                ("route_names", route_regions),
                ("route_total", route_total.to_string()),
                ("route_kills", route_kills.to_string()),
            ])
        };

        let template = self
            .config
            .as_ref()
            .and_then(|c| c.overlay.as_ref())
            .and_then(|o| o.display_text.as_deref())
            .unwrap_or(DEFAULT_DISPLAY_TEXT);

        let lines = format_display_text(template, &vars);
        for line in lines.clone() {
            ui.text(line);
        }

        let header_h = ui.text_line_height_with_spacing() * lines.len() as f32 + 8.0;
        if Self::is_click_in_header(ui, header_h) {
            let now = Instant::now();
            if now.duration_since(self.last_toggle_time) > Duration::from_millis(300) {
                self.full_mode = false;
                self.last_toggle_time = now;
                debug_log!("[ignite_overlay] Clicked header — collapsing overlay");
            }
        }

        let avail = ui.content_region_avail();
        let child = ui
            .child_window("BossListRegion")
            .size(avail)
            .border(false)
            .begin();

        if let Some(_child_token) = child {
            if let Some(data) = self.boss_regions.as_ref() {
                if let Ok(state) = self.state.read() {
                    let flags = &state.event_flags;
                    let active_region = state.active_region_name.clone();

                    for region in data {
                        let defeated = region
                            .bosses
                            .iter()
                            .filter(|b| *flags.get(&b.flag_id).unwrap_or(&false))
                            .count();
                        let total = region.bosses.len();

                        let label = if region.region_name == active_region {
                            format!("▶ {} ({}/{})", region.region_name, defeated, total)
                        } else {
                            format!("{} ({}/{})", region.region_name, defeated, total)
                        };

                        if let Some(_t) = ui
                            .tree_node_config(label)
                            .flags(imgui::TreeNodeFlags::SPAN_AVAIL_WIDTH)
                            .push()
                        {
                            for boss in &region.bosses {
                                let mut checked = *flags.get(&boss.flag_id).unwrap_or(&false);
                                ui.checkbox(
                                    &format!(
                                        "{}{}",
                                        boss.boss,
                                        if boss.place.is_empty() { "" } else { " " }
                                    ),
                                    &mut checked,
                                );
                            }
                        }
                    }
                }
            }

            _child_token.end();
        }
    }

    fn measure_closed_size(&mut self, ui: &Ui) -> (f32, f32) {
        self.write_igt();

        let (deaths, shards, runes, region_name, phase_name) = if let Ok(state) = self.state.read()
        {
            let region_name = state.active_region_name.clone();
            let phase_name = if let (Some(schedule), Some(phase_idx)) =
                (self.region_schedule.as_ref(), state.active_phase_index)
            {
                schedule
                    .phases
                    .get(phase_idx)
                    .map(|p| p.name.clone())
                    .unwrap_or_default()
            } else {
                String::new()
            };

            (
                state.death_count,
                state.key_item_quantity,
                state.great_runes,
                region_name,
                phase_name,
            )
        } else {
            (0, 0, 0, String::new(), String::new())
        };

        let (defeated, total) = self.build_current_region_totals(&region_name);
        let route_regions = self.build_route_string(&region_name);
        let (route_kills, route_total) = self.build_route_totals();

        let vars = HashMap::from([
            ("kills", defeated.to_string()),
            ("total", total.to_string()),
            ("counted_kills", defeated.to_string()),
            ("counted_total", total.to_string()),
            ("deaths", deaths.to_string()),
            ("igt", self.timer_buf.clone()),
            ("shards", shards.to_string()),
            ("runes", runes.to_string()),
            ("region", region_name),
            ("phase", phase_name),
            ("route", route_regions.clone()),
            ("route_names", route_regions),
            ("route_total", route_total.to_string()),
            ("route_kills", route_kills.to_string()),
        ]);

        let template = self
            .config
            .as_ref()
            .and_then(|c| c.overlay.as_ref())
            .and_then(|o| o.display_text.as_deref())
            .unwrap_or(DEFAULT_DISPLAY_TEXT);

        let lines = format_display_text(template, &vars);

        let pad = unsafe { ui.style().window_padding };

        let max_w = lines
            .iter()
            .map(|l| ui.calc_text_size(l)[0])
            .fold(0.0, f32::max);

        let total_h = pad[1] * 2.0 + ui.text_line_height_with_spacing() * lines.len() as f32;
        let total_w = pad[0] * 2.0 + max_w;

        (total_w.ceil() + 4.0, total_h.ceil())
    }

    fn write_igt(&mut self) {
        self.timer_buf.clear();

        if let Ok(seconds) = self.igt.read() {
            let raw_seconds = *seconds as i64;
            let prep_seconds = (self.prep_time_ms as i64) / 1000;
            let timer_target_seconds = (self.timer_target_ms as i64) / 1000;

            let display_seconds: i64 = match self.timer_mode {
                TimerMode::Regular => raw_seconds,
                TimerMode::Timer => timer_target_seconds - raw_seconds,
                TimerMode::Prep => raw_seconds - prep_seconds,
                TimerMode::PrepTimer => {
                    if raw_seconds < prep_seconds {
                        raw_seconds - prep_seconds
                    } else {
                        let after_prep = raw_seconds - prep_seconds;
                        timer_target_seconds - after_prep
                    }
                }
            };

            let is_negative = display_seconds < 0;
            let total_seconds = display_seconds.abs();

            if total_seconds > 86_400 {
                let days = total_seconds / 86_400;
                let rem_d = total_seconds % 86_400;
                let hours = rem_d / 3_600;
                let rem_h = rem_d % 3_600;
                let minutes = rem_h / 60;
                let seconds = rem_h % 60;

                if is_negative {
                    let _ = write!(
                        self.timer_buf,
                        "-{:02}:{:02}:{:02}:{:02}",
                        days, hours, minutes, seconds
                    );
                } else {
                    let _ = write!(
                        self.timer_buf,
                        "{:02}:{:02}:{:02}:{:02}",
                        days, hours, minutes, seconds
                    );
                }
            } else {
                let hours = total_seconds / 3_600;
                let rem_h = total_seconds % 3_600;
                let minutes = rem_h / 60;
                let seconds = rem_h % 60;

                if is_negative {
                    let _ = write!(
                        self.timer_buf,
                        "-{:02}:{:02}:{:02}",
                        hours, minutes, seconds
                    );
                } else {
                    let _ = write!(
                        self.timer_buf,
                        "{:02}:{:02}:{:02}",
                        hours, minutes, seconds
                    );
                }
            }
        }
    }

    fn config_dim(&self) -> (f32, f32) {
        self.config
            .as_ref()
            .and_then(|c| c.style.as_ref())
            .and_then(|s| s.panel_dim)
            .unwrap_or([0.15, 0.90])
            .into()
    }

    fn render_centered_text_block(ui: &imgui::Ui, lines: &[String]) {
        let line_h = ui.text_line_height_with_spacing();
        let total_h = line_h * lines.len() as f32;

        let avail_h = ui.content_region_avail()[1];
        let y_offset = (avail_h - total_h) * 0.5;
        if y_offset > 0.0 {
            let mut pos = ui.cursor_pos();
            pos[1] += y_offset;
            ui.set_cursor_pos(pos);
        }

        for line in lines {
            ui.text(line);
        }
    }

    fn simulate_mouse_click(imgui: &mut imgui::Context) {
        use imgui::MouseButton;

        let io = imgui.io_mut();
        io.add_mouse_button_event(MouseButton::Left, true);
        io.add_mouse_button_event(MouseButton::Left, false);

        debug_log!("[ignite_overlay] Simulated mouse click");
    }

    fn is_click_in_header(ui: &imgui::Ui, header_height: f32) -> bool {
        let io = ui.io();
        if !io.mouse_down[0] {
            return false;
        }
        let mouse_pos = io.mouse_pos;
        let win_pos = ui.window_pos();
        let win_size = ui.window_size();

        let inside_x = mouse_pos[0] >= win_pos[0] && mouse_pos[0] <= win_pos[0] + win_size[0];
        let inside_y = mouse_pos[1] >= win_pos[1] && mouse_pos[1] <= win_pos[1] + header_height;
        inside_x && inside_y
    }
}

impl ImguiRenderLoop for EROverlayUi {
    fn initialize(&mut self, imgui: &mut imgui::Context, ctx: &mut dyn hudhook::RenderContext) {
        #[cfg(debug_assertions)]
        attach_console();

        debug_log!("[ignite_overlay] Initializing overlay...");

        if let Some(cfg) = &self.config {
            apply_style_config(imgui, cfg);
            apply_common_config(imgui, cfg, ctx);
        }

        let Some(boss_regions) = self.boss_regions.clone() else {
            debug_log!("[ignite_overlay] No boss regions loaded; monitor not started.");
            return;
        };

        let Some(region_schedule) = self.region_schedule.clone() else {
            debug_log!("[ignite_overlay] No region schedule loaded; monitor not started.");
            return;
        };

        if let Some(dir) = self.dll_dir.as_ref() {
            if let Some(saved) = load_run_state(dir, &self.seed_id) {
                if let Ok(mut state) = self.state.write() {
                    state.counted_flags = saved.counted_flags;
                    state.cumulative_counted_kills = saved.cumulative_counted_kills;
                    state.counted_kills = state.cumulative_counted_kills;

                    debug_log!(
                        "[ignite_overlay] Restored saved run state for seed '{}' with {} counted kills",
                        self.seed_id,
                        state.cumulative_counted_kills
                    );
                }
            }
        }

        let key_item_id = 2008021;

        let Some(save_dir) = self.dll_dir.clone() else {
            debug_log!("[ignite_overlay] No DLL dir available; monitor not started.");
            return;
        };

        start_game_monitor(
            self.state.clone(),
            self.igt.clone(),
            boss_regions,
            region_schedule,
            key_item_id,
            100,
            self.monitor_stop.clone(),
            save_dir,
            self.seed_id.clone(),
        );

        debug_log!("[ignite_overlay] Game monitor started successfully.");
    }

    fn before_render(
        &mut self,
        imgui: &mut imgui::Context,
        _ctx: &mut dyn hudhook::RenderContext,
    ) {
        try_auto_activate_nearby_grace();

        let io = imgui.io();
        let now = std::time::Instant::now();

        if let Some(keys) = &self.click_action_keys {
            if keys.iter().all(|&k| io.keys_down[k as usize]) {
                if now.duration_since(self.last_click_time)
                    > std::time::Duration::from_millis(200)
                {
                    Self::simulate_mouse_click(imgui);
                    self.last_click_time = now;
                }
            }
        }
    }

    fn render(&mut self, ui: &mut imgui::Ui) {
        if let Some(keys) = self.toggle_full_mode_keys.as_ref() {
            if keys.iter().all(|&k| ui.is_key_pressed(k)) {
                self.full_mode = !self.full_mode;
                debug_log!("[ignite_overlay] full_mode toggled -> {}", self.full_mode);
            }
        }

        let io = ui.io();
        let (screen_w, screen_h) = (io.display_size[0], io.display_size[1]);

        let (cfg_w, cfg_h, cfg_x, cfg_y) = {
            let (w_ratio, h_ratio) = self.config_dim();
            let [x_off, y_off] = self
                .config
                .as_ref()
                .and_then(|c| c.style.as_ref())
                .and_then(|s| s.panel_pos)
                .unwrap_or(DEFAULT_PANEL_POS);

            let w = screen_w * w_ratio;
            let h = screen_h * h_ratio;

            let x = if x_off < 0.0 { screen_w - w + x_off } else { x_off };
            let y = if y_off < 0.0 { screen_h - h + y_off } else { y_off };
            (w, h, x, y)
        };

        if self.full_mode {
            ui.window("Ignite HUD")
                .position([cfg_x, cfg_y], imgui::Condition::Always)
                .size([cfg_w, cfg_h], imgui::Condition::Always)
                .flags(
                    imgui::WindowFlags::NO_TITLE_BAR
                        | imgui::WindowFlags::NO_RESIZE
                        | imgui::WindowFlags::NO_MOVE,
                )
                .build(|| {
                    if let Some(err) = &self.config_error {
                        let _c =
                            ui.push_style_color(imgui::StyleColor::Text, [1.0, 0.2, 0.2, 1.0]);
                        ui.text("⚠ Failed to load config:");
                        ui.text_wrapped(err);
                        return;
                    }
                    self.render_open(ui);
                });
        } else {
            let [x_off, y_off] = self
                .config
                .as_ref()
                .and_then(|c| c.style.as_ref())
                .and_then(|s| s.panel_pos)
                .unwrap_or(DEFAULT_PANEL_POS);

            let (closed_w, closed_h) = self.measure_closed_size(ui);

            let x = if x_off < 0.0 {
                screen_w - closed_w + x_off
            } else {
                x_off
            };
            let y = if y_off < 0.0 {
                screen_h - closed_h + y_off
            } else {
                y_off
            };

            ui.window("Ignite HUD")
                .position([x, y], imgui::Condition::Always)
                .size([closed_w, closed_h], imgui::Condition::Always)
                .flags(
                    imgui::WindowFlags::NO_TITLE_BAR
                        | imgui::WindowFlags::NO_RESIZE
                        | imgui::WindowFlags::NO_MOVE,
                )
                .build(|| {
                    if let Some(err) = &self.config_error {
                        let _c =
                            ui.push_style_color(imgui::StyleColor::Text, [1.0, 0.2, 0.2, 1.0]);
                        ui.text("⚠ Failed to load config:");
                        ui.text_wrapped(err);
                        return;
                    }
                    self.render_closed(ui);
                });
        }
    }
}

impl Drop for EROverlayUi {
    fn drop(&mut self) {
        self.monitor_stop
            .store(true, std::sync::atomic::Ordering::SeqCst);

        std::thread::sleep(std::time::Duration::from_millis(150));

        debug_log!("[ignite_overlay] 🔻 Teardown: monitor thread stop signal sent.");
    }
}