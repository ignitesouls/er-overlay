use std::{
    collections::HashMap,
    fmt::Write,
    sync::{Arc, RwLock, atomic::AtomicBool},
    time::{Duration, Instant},
};

use hudhook::{ImguiRenderLoop, eject};
use imgui::{Key, Ui};

use crate::{
    debug_log,
    er::{grace::try_auto_activate_nearby_grace, item_spawn::start_test_weapon_grant},
    overlay::{
        core::start_game_monitor,
        data::{
            AppState, BossRegions, EventFlagSchedule, create_state, load_event_flag_schedule,
            load_localized_boss_data,
        },
        style::{
            DEFAULT_DISPLAY_TEXT, DEFAULT_PANEL_POS, IgniteConfig, TimerMode, apply_common_config,
            apply_style_config, parse_key_combo, read_config,
        },
    },
    util::{introspection::get_dll_directory, text_formatter::format_display_text},
};

pub struct EROverlayUi {
    last_click_time: Instant,
    last_toggle_time: Instant,
    last_unload_time: Instant,
    last_reset_time: Instant,
    timer_buf: String,
    full_mode: bool,

    config: Option<IgniteConfig>,
    config_error: Option<String>,

    toggle_full_mode_keys: Option<Vec<Key>>,
    unload_keys: Option<Vec<Key>>,
    click_action_keys: Option<Vec<imgui::Key>>,
    reset_run_keys: Option<Vec<Key>>,

    state: Arc<RwLock<AppState>>,
    igt: Arc<RwLock<u32>>,
    boss_regions: Option<BossRegions>,
    event_flag_schedule: Option<EventFlagSchedule>,

    timer_mode: TimerMode,
    prep_time_ms: u32,
    timer_target_ms: u32,

    monitor_stop: Arc<AtomicBool>,
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

        let unload_keys = config
            .as_ref()
            .and_then(|c| c.input.as_ref())
            .and_then(|i| i.unload.clone())
            .map(|combo| parse_key_combo(&combo));

        let click_action_keys = config
            .as_ref()
            .and_then(|c| c.input.as_ref())
            .and_then(|i| i.click_action.clone())
            .map(|s| parse_key_combo(&s));

        let reset_run_keys = config
            .as_ref()
            .and_then(|c| c.input.as_ref())
            .and_then(|i| i.reset_run.clone())
            .map(|combo| parse_key_combo(&combo));

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

        let event_flag_file = config
            .as_ref()
            .and_then(|c| c.event_flags.as_ref())
            .and_then(|s| s.file.as_ref())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or("player_event_flags.json");

        let dll_dir = get_dll_directory();

        let boss_regions = dll_dir
            .as_ref()
            .and_then(|dir| load_localized_boss_data(dir, language, data_file));

        let event_flag_schedule = dll_dir
            .as_ref()
            .and_then(|dir| load_event_flag_schedule(dir, event_flag_file));

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
            last_unload_time: Instant::now(),
            last_reset_time: Instant::now(),
            timer_buf: String::with_capacity(32),
            full_mode: false,
            config,
            config_error,
            toggle_full_mode_keys,
            unload_keys,
            click_action_keys,
            reset_run_keys,
            state: create_state(),
            igt: Arc::new(RwLock::new(0)),
            boss_regions,
            event_flag_schedule,
            timer_mode,
            prep_time_ms,
            timer_target_ms,
            monitor_stop: Arc::new(AtomicBool::new(false)),
        }
    }

    fn collect_boss_flags(&self) -> Vec<i32> {
        self.boss_regions
            .as_ref()
            .map(|regions| {
                regions
                    .iter()
                    .flat_map(|region| region.bosses.iter().map(|boss| boss.flag_id))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn boss_total(&self) -> usize {
        self.boss_regions
            .as_ref()
            .map(|regions| regions.iter().map(|region| region.bosses.len()).sum())
            .unwrap_or(0)
    }
    fn snapshot_vars(&mut self) -> HashMap<&'static str, String> {
        self.write_igt();

        let boss_total = self.boss_total();
        let (defeated, deaths, shards, runes, current_events) = if let Ok(state) = self.state.read()
        {
            (
                state.event_flags.values().filter(|&&flag| flag).count(),
                state.death_count,
                state.key_item_quantity,
                state.great_runes,
                state.current_events.clone(),
            )
        } else {
            (0, 0, 0, 0, String::new())
        };

        HashMap::from([
            ("kills", defeated.to_string()),
            ("total", boss_total.to_string()),
            ("counted_kills", defeated.to_string()),
            ("counted_total", boss_total.to_string()),
            ("deaths", deaths.to_string()),
            ("igt", self.timer_buf.clone()),
            ("shards", shards.to_string()),
            ("runes", runes.to_string()),
            ("current_events", current_events),
        ])
    }

    fn template_lines(&mut self) -> Vec<String> {
        let vars = self.snapshot_vars();
        let template = self
            .config
            .as_ref()
            .and_then(|c| c.overlay.as_ref())
            .and_then(|o| o.display_text.as_deref())
            .unwrap_or(DEFAULT_DISPLAY_TEXT);
        format_display_text(template, &vars)
    }

    fn render_closed(&mut self, ui: &Ui) {
        let lines = self.template_lines();
        Self::render_metric_block(ui, &lines, true);

        let total_h = ui.text_line_height_with_spacing() * lines.len() as f32 + 8.0;
        if Self::is_click_in_header(ui, total_h) {
            let now = Instant::now();
            if now.duration_since(self.last_toggle_time) > Duration::from_millis(300) {
                self.full_mode = true;
                self.last_toggle_time = now;
                debug_log!("[ignite_overlay] Clicked compact overlay - expanding");
            }
        }
    }

    fn render_open(&mut self, ui: &Ui) {
        let lines = self.template_lines();
        Self::render_metric_block(ui, &lines, false);

        let header_h = ui.text_line_height_with_spacing() * lines.len() as f32 + 8.0;
        if Self::is_click_in_header(ui, header_h) {
            let now = Instant::now();
            if now.duration_since(self.last_toggle_time) > Duration::from_millis(300) {
                self.full_mode = false;
                self.last_toggle_time = now;
                debug_log!("[ignite_overlay] Clicked header - collapsing overlay");
            }
        }

        self.render_boss_list(ui);
    }

    fn render_boss_list(&self, ui: &Ui) {
        ui.spacing();
        ui.separator();
        ui.spacing();
        ui.text_colored([0.72, 0.78, 0.82, 0.92], "BOSSES");
        ui.spacing();

        let Some(regions) = self.boss_regions.as_ref() else {
            ui.text("Boss data not loaded");
            return;
        };

        let Ok(state) = self.state.read() else {
            ui.text("Boss state not ready");
            return;
        };

        let child = ui
            .child_window("BossListRegion")
            .size(ui.content_region_avail())
            .border(false)
            .begin();

        if let Some(_child_token) = child {
            for group in regions {
                let defeated = group
                    .bosses
                    .iter()
                    .filter(|boss| *state.event_flags.get(&boss.flag_id).unwrap_or(&false))
                    .count();
                let total = group.bosses.len();

                let region_label = format!("{}  {}/{}", group.region_name, defeated, total);
                if let Some(_tree) = ui
                    .tree_node_config(region_label)
                    .flags(imgui::TreeNodeFlags::SPAN_AVAIL_WIDTH)
                    .push()
                {
                    for boss in &group.bosses {
                        let mut checked = *state.event_flags.get(&boss.flag_id).unwrap_or(&false);
                        let label = if boss.place.is_empty() {
                            boss.boss.clone()
                        } else {
                            format!("{} - {}", boss.boss, boss.place)
                        };
                        ui.checkbox(&label, &mut checked);
                    }
                }
            }

            _child_token.end();
        }
    }

    fn measure_closed_size(&mut self, ui: &Ui) -> (f32, f32) {
        let lines = self.template_lines();
        let pad = unsafe { ui.style().window_padding };
        let (label_w, value_w) = Self::measure_metric_columns(ui, &lines);
        let total_h = pad[1] * 2.0 + ui.text_line_height_with_spacing() * lines.len() as f32;
        let total_w = pad[0] * 2.0 + label_w + 12.0 + value_w;
        (total_w.ceil().max(260.0), total_h.ceil() + 4.0)
    }

    fn write_igt(&mut self) {
        self.timer_buf.clear();

        if let Ok(ms) = self.igt.read() {
            let raw_ms = *ms as i64;
            let prep_ms = self.prep_time_ms as i64;
            let timer_target_ms = self.timer_target_ms as i64;

            let display_ms = match self.timer_mode {
                TimerMode::Regular => raw_ms,
                TimerMode::Timer => timer_target_ms - raw_ms,
                TimerMode::Prep => raw_ms - prep_ms,
                TimerMode::PrepTimer => {
                    if raw_ms < prep_ms {
                        raw_ms - prep_ms
                    } else {
                        timer_target_ms - (raw_ms - prep_ms)
                    }
                }
            };

            let total_seconds = display_ms / 1000;
            let is_negative = total_seconds < 0;
            let total_seconds = total_seconds.abs();

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
                    let _ = write!(self.timer_buf, "{:02}:{:02}:{:02}", hours, minutes, seconds);
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

    fn metric_parts(line: &str) -> (String, String) {
        line.split_once(':')
            .map(|(label, value)| {
                let value = value.trim();
                (
                    format!("{}:", label.trim()),
                    if value.is_empty() {
                        "None".to_string()
                    } else {
                        value.to_string()
                    },
                )
            })
            .unwrap_or_else(|| (String::new(), line.trim().to_string()))
    }

    fn measure_metric_columns(ui: &imgui::Ui, lines: &[String]) -> (f32, f32) {
        lines
            .iter()
            .fold((0.0f32, 0.0f32), |(label_max, value_max), line| {
                let (label, value) = Self::metric_parts(line);
                (
                    label_max.max(ui.calc_text_size(&label)[0]),
                    value_max.max(ui.calc_text_size(&value)[0]),
                )
            })
    }

    fn render_metric_block(ui: &imgui::Ui, lines: &[String], centered: bool) {
        let line_h = ui.text_line_height_with_spacing();
        if centered {
            let total_h = line_h * lines.len() as f32;
            let avail_h = ui.content_region_avail()[1];
            let y_offset = (avail_h - total_h) * 0.5;
            if y_offset > 0.0 {
                let mut pos = ui.cursor_pos();
                pos[1] += y_offset;
                ui.set_cursor_pos(pos);
            }
        }

        let (label_w, value_w) = Self::measure_metric_columns(ui, lines);
        for line in lines {
            Self::render_metric_line(ui, line, centered, label_w, value_w);
        }
    }

    fn render_metric_line(
        ui: &imgui::Ui,
        line: &str,
        _centered: bool,
        label_width: f32,
        _value_width: f32,
    ) {
        let (label, value) = Self::metric_parts(line);
        let label_lower = label.to_ascii_lowercase();
        let is_effects = label_lower.contains("special effects");
        let is_time = label_lower.contains("time");
        let value_color = if is_effects && value != "None" {
            [0.98, 0.76, 0.34, 1.0]
        } else if is_time {
            [0.72, 0.90, 1.0, 1.0]
        } else {
            [0.92, 0.96, 0.98, 0.98]
        };

        let label_color = [0.56, 0.64, 0.70, 0.92];
        let muted_color = [0.52, 0.58, 0.62, 0.88];
        let gap = 12.0;
        let row_x = ui.cursor_pos()[0];
        let row_y = ui.cursor_pos()[1];
        if !label.is_empty() {
            ui.set_cursor_pos([row_x, row_y]);
            ui.text_colored(label_color, &label);
            ui.same_line();
        }

        ui.set_cursor_pos([row_x + label_width + gap, row_y]);
        let color = if value == "None" {
            muted_color
        } else {
            value_color
        };
        let _text_color = ui.push_style_color(imgui::StyleColor::Text, color);
        if is_effects {
            ui.text_wrapped(&value);
        } else {
            ui.text(&value);
        }
    }

    fn simulate_mouse_click(imgui: &mut imgui::Context) {
        use imgui::MouseButton;
        let io = imgui.io_mut();
        io.add_mouse_button_event(MouseButton::Left, true);
        io.add_mouse_button_event(MouseButton::Left, false);
        debug_log!("[ignite_overlay] Simulated mouse click");
    }

    fn combo_down(io: &imgui::Io, keys: &[Key]) -> bool {
        !keys.is_empty() && keys.iter().all(|&key| io.keys_down[key as usize])
    }

    fn combo_pressed(ui: &imgui::Ui, keys: &[Key]) -> bool {
        !keys.is_empty()
            && keys.iter().all(|&key| ui.io().keys_down[key as usize])
            && keys.iter().any(|&key| ui.is_key_pressed(key))
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
        debug_log!("[ignite_overlay] Initializing overlay...");

        if let Some(cfg) = &self.config {
            apply_style_config(imgui, cfg);
            apply_common_config(imgui, cfg, ctx);

            if cfg.experimental_item_spawn_enabled() {
                if let Some(weapon_id) = cfg.test_weapon_id() {
                    start_test_weapon_grant(weapon_id);
                } else {
                    debug_log!(
                        "[ignite_overlay] Experimental item spawn enabled, but no test_weapon_id configured"
                    );
                }
            }
        }

        if self
            .config
            .as_ref()
            .is_some_and(|cfg| !cfg.boss_tracking_enabled())
        {
            debug_log!("[ignite_overlay] Boss tracking disabled by config; monitor not started.");
            return;
        }

        let flag_ids = self.collect_boss_flags();
        debug_log!("[ignite_overlay] Loaded {} boss flags", flag_ids.len());

        let interval_event_flags_enabled = self
            .config
            .as_ref()
            .is_some_and(|cfg| cfg.interval_event_flags_enabled());

        start_game_monitor(
            self.state.clone(),
            self.igt.clone(),
            flag_ids,
            interval_event_flags_enabled,
            self.event_flag_schedule.clone(),
            2008021,
            100,
            self.monitor_stop.clone(),
        );

        debug_log!("[ignite_overlay] Game monitor started successfully.");
    }

    fn before_render(&mut self, imgui: &mut imgui::Context, _ctx: &mut dyn hudhook::RenderContext) {
        let io = imgui.io();
        let now = Instant::now();

        if self
            .config
            .as_ref()
            .is_some_and(|cfg| cfg.auto_grace_enabled())
        {
            try_auto_activate_nearby_grace();
        }

        if let Some(keys) = &self.unload_keys {
            if Self::combo_down(io, keys)
                && now.duration_since(self.last_unload_time) > Duration::from_millis(500)
            {
                self.last_unload_time = now;
                debug_log!("[ignite_overlay] Unload shortcut pressed; ejecting overlay");
                eject();
            }
        }

        let should_reset = self.reset_run_keys.as_ref().is_some_and(|keys| {
            Self::combo_down(io, keys)
                && now.duration_since(self.last_reset_time) > Duration::from_millis(500)
        });

        if should_reset {
            self.last_reset_time = now;
            if let Ok(mut state) = self.state.write() {
                state.event_flags.clear();
            }
            debug_log!("[ignite_overlay] Cleared local boss flag cache");
        }

        if let Some(keys) = &self.click_action_keys {
            if Self::combo_down(io, keys)
                && now.duration_since(self.last_click_time) > Duration::from_millis(200)
            {
                Self::simulate_mouse_click(imgui);
                self.last_click_time = now;
            }
        }
    }

    fn render(&mut self, ui: &mut imgui::Ui) {
        if self
            .config
            .as_ref()
            .is_some_and(|cfg| !cfg.overlay_enabled())
        {
            return;
        }

        if let Some(keys) = self.toggle_full_mode_keys.as_ref() {
            if Self::combo_pressed(ui, keys) {
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
            let x = if x_off < 0.0 {
                screen_w - w + x_off
            } else {
                x_off
            };
            let y = if y_off < 0.0 {
                screen_h - h + y_off
            } else {
                y_off
            };
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
                        let _c = ui.push_style_color(imgui::StyleColor::Text, [1.0, 0.2, 0.2, 1.0]);
                        ui.text("Failed to load config:");
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
                        let _c = ui.push_style_color(imgui::StyleColor::Text, [1.0, 0.2, 0.2, 1.0]);
                        ui.text("Failed to load config:");
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
        thread_sleep_150ms();
        debug_log!("[ignite_overlay] Teardown: monitor thread stop signal sent.");
    }
}

fn thread_sleep_150ms() {
    std::thread::sleep(Duration::from_millis(150));
}
