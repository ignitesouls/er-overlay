use std::{
    fmt::Write,
    sync::{Arc, RwLock, atomic::{AtomicBool}},
    time::{Instant, Duration},
    collections::HashMap
};

use hudhook::ImguiRenderLoop;
use imgui::{Ui, Key};


use crate::{
    debug_log,
    ingest::{IngestSettings, SharedIngestStatus, create_status, start_reporter, tally_line},
    overlay::{core::start_game_monitor, data::{AppState, BossRegions, create_state, load_localized_boss_data},
    style::{DEFAULT_DISPLAY_TEXT, DEFAULT_PANEL_POS, IgniteConfig, TimerMode, apply_common_config, apply_style_config, parse_key_combo, read_config}},
    util::{debug::attach_console, introspection::get_dll_directory, text_formatter::format_display_text}
};

//
// ----------------------------------------------------
// EROverlayUi Definition
// ----------------------------------------------------
//

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

    timer_mode: TimerMode,
    prep_time_ms: u32,
    timer_target_ms: u32,

    monitor_stop: Arc<AtomicBool>,

    /// `None` unless `[ingest]` has both a url and a token.
    ingest_settings: Option<IngestSettings>,
    ingest_status: SharedIngestStatus,
    show_ingest_tally: bool,
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
            .map(|s| parse_key_combo(&s)); // parse_key_combo returns Vec<Key>

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

        let boss_regions = get_dll_directory()
            .and_then(|dir| load_localized_boss_data(&dir, language, data_file));

        let timer_mode = config
            .as_ref()
            .and_then(|c| c.timer.as_ref())
            .map(|t| t.mode)
            .unwrap_or(TimerMode::Regular);

        let prep_time_ms = config
            .as_ref()
            .and_then(|c| c.timer.as_ref())
            .and_then(|t| t.prep_minutes)
            .unwrap_or(0) * 60_000;

        let timer_target_ms = config
            .as_ref()
            .and_then(|c| c.timer.as_ref())
            .and_then(|t| t.timer_minutes)
            .unwrap_or(0) * 60_000;

        let ingest_settings = IngestSettings::from_config(
            config.as_ref().and_then(|c| c.ingest.as_ref())
        );

        // The tally line can only ever appear when reporting is on, and it
        // stays hidden until the server reports a live match.
        let show_ingest_tally = ingest_settings.is_some()
            && config
                .as_ref()
                .and_then(|c| c.overlay.as_ref())
                .and_then(|o| o.show_ingest_tally)
                .unwrap_or(true);

        Self {
            last_click_time: Instant::now(),
            last_toggle_time: Instant::now(),
            timer_buf: String::with_capacity(32),
            full_mode: false,
            config,
            config_error,
            toggle_full_mode_keys,
            click_action_keys,
            boss_regions,
            state: create_state(),
            igt: Arc::new(RwLock::new(0)),
            timer_mode,
            prep_time_ms,
            timer_target_ms,
            monitor_stop: Arc::new(AtomicBool::new(false)),
            ingest_settings,
            ingest_status: create_status(),
            show_ingest_tally,
        }
    }

    /// The HUD text block: the configured display template, plus the
    /// kill-reporting tally when there is one to show.
    ///
    /// Shared by both render modes and by the compact-mode size measurement, so
    /// the window is always sized for exactly what gets drawn.
    fn build_lines(&self) -> Vec<String> {
        let (defeated, total, deaths, shards, runes) = if let Ok(state) = self.state.read() {
            (
                state.event_flags.values().filter(|&&b| b).count(),
                state.event_flags.len(),
                state.death_count,
                state.key_item_quantity,
                state.great_runes,
            )
        } else {
            (0, 0, 0, 0, 0)
        };

        let vars = HashMap::from([
            ("kills", defeated.to_string()),
            ("total", total.to_string()),
            ("deaths", deaths.to_string()),
            ("igt", self.timer_buf.clone()),
            ("shards", shards.to_string()),
            ("runes", runes.to_string()),
        ]);

        let template = self.config
            .as_ref()
            .and_then(|c| c.overlay.as_ref())
            .and_then(|o| o.display_text.as_deref())
            .unwrap_or(DEFAULT_DISPLAY_TEXT);

        let mut lines = format_display_text(template, &vars);

        if self.show_ingest_tally {
            if let Ok(status) = self.ingest_status.read() {
                if let Some(line) = tally_line(&status) {
                    // Padded to match how `format_display_text` emits lines.
                    lines.push(format!(" {} ", line));
                }
            }
        }

        lines
    }

    fn render_closed(&mut self, ui: &Ui) {
        self.write_igt();
        let lines = self.build_lines();
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
        let lines = self.build_lines();
        for line in &lines {
            ui.text(line);
        }

        // In expanded mode there is room to say *why* the tally is stale.
        if self.show_ingest_tally {
            if let Ok(status) = self.ingest_status.read() {
                if let Some(err) = status.last_error.as_deref() {
                    let _c = ui.push_style_color(imgui::StyleColor::Text, [1.0, 0.6, 0.2, 1.0]);
                    ui.text(format!(" ⚠ last report failed: {} ", err));
                }
            }
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
        let child = ui.child_window("BossListRegion")
            .size(avail)
            .border(false)
            .begin();

        if let Some(_child_token) = child {
            // --- Scrollable boss list content ---
            if let Some(data) = self.boss_regions.as_ref() {
                if let Ok(state) = self.state.read() {
                    let flags = &state.event_flags;
                    for region in data {
                        let defeated = region
                            .bosses
                            .iter()
                            .filter(|b| *flags.get(&b.flag_id).unwrap_or(&false))
                            .count();
                        let total = region.bosses.len();

                        if let Some(_t) = ui
                            .tree_node_config(format!("{} ({}/{})", region.region_name, defeated, total))
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
        let lines = self.build_lines();

        // Split lines for accurate height calc
        let pad = unsafe { ui.style().window_padding };

        let max_w = lines.iter()
            .map(|l| ui.calc_text_size(l)[0])
            .fold(0.0, f32::max);

        let total_h = pad[1] * 2.0 + ui.text_line_height_with_spacing() * lines.len() as f32;
        let total_w = pad[0] * 2.0 + max_w;

        (total_w.ceil() + 4.0, total_h.ceil())
    }


    //
    // ----------------------------------------------------
    // Layout & Timer
    // ----------------------------------------------------
    //

    /* 
    fn write_igt(&mut self) {
        self.timer_buf.clear();
        if let Ok(ms) = self.igt.read() {


            let raw_ms = *ms as i64;
            let prep_ms = self.prep_time_ms as i64;
            let timer_target = self.timer_target_ms as i64;

            let display_ms = match self.timer_mode {

                TimerMode::Regular => raw_ms,

                TimerMode::Timer => {
                    if raw_ms >= self.timer_target_ms {
                        0
                    } else {
                        self.timer_target_ms - raw_ms
                    }
                }

                TimerMode::Prep => {
                    if raw_ms < self.prep_time_ms {
                        self.prep_time_ms - raw_ms
                    } else {
                        raw_ms - self.prep_time_ms
                    }
                }

                TimerMode::PrepTimer => {
                    if raw_ms < self.prep_time_ms {
                        self.prep_time_ms - raw_ms
                    } else {
                        let after_prep = raw_ms - self.prep_time_ms;

                        if after_prep >= self.timer_target_ms {
                            0
                        } else {
                            self.timer_target_ms - after_prep
                        }
                    }
                }
            };

            let total_seconds = display_ms / 1000;
            let is_negative = total_seconds < 0;
            let total_seconds = total_seconds.abs();
            
            // let total = *ms / 1000;

            if total > 86400 {
                let (days, rem_d) = (total / 86_400, total % 86_400);
                let (hours, rem_h) = (rem_d / 3_600, rem_d % 3_600);
                let (minutes, seconds) = (rem_h / 60, rem_h % 60);
                let _ = write!(self.timer_buf, "{:02}:{:02}:{:02}:{:02}", days, hours, minutes, seconds);
            } else {
                let (hours, rem_h) = (total / 3_600, total % 3_600);
                let (minutes, seconds) = (rem_h / 60, rem_h % 60);
                let _ = write!(self.timer_buf, "{:02}:{:02}:{:02}", hours, minutes, seconds);
            }
        }
    }
    */

    fn write_igt(&mut self) {
        self.timer_buf.clear();

        if let Ok(ms) = self.igt.read() {

            // Convert everything to signed for negative support
            let raw_ms = *ms as i64;
            let prep_ms = self.prep_time_ms as i64;
            let timer_target = self.timer_target_ms as i64;

            // Determine what should be displayed
            let display_ms: i64 = match self.timer_mode {

                TimerMode::Regular => raw_ms,

                TimerMode::Timer => {
                    timer_target - raw_ms
                }

                TimerMode::Prep => {
                    // Negative during prep, positive afterward
                    raw_ms - prep_ms
                }

                TimerMode::PrepTimer => {
                    if raw_ms < prep_ms {
                        // Negative during prep
                        raw_ms - prep_ms
                    } else {
                        let after_prep = raw_ms - prep_ms;
                        timer_target - after_prep
                    }
                }
            };

            // Convert to seconds
            let total_seconds = display_ms / 1000;
            let is_negative = total_seconds < 0;
            let total_seconds = total_seconds.abs();

            // Format as DD:HH:MM:SS if over a day
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
        // Compute total height of all lines (including line spacing)
        let line_h = ui.text_line_height_with_spacing();
        let total_h = line_h * lines.len() as f32;

        // Compute vertical offset for centering in the available region
        let avail_h = ui.content_region_avail()[1];
        let y_offset = (avail_h - total_h) * 0.5;
        if y_offset > 0.0 {
            let mut pos = ui.cursor_pos();
            pos[1] += y_offset;
            ui.set_cursor_pos(pos);
        }

        // Draw lines normally (left-aligned)
        for line in lines {
            ui.text(line);
        }
    }

    fn simulate_mouse_click(imgui: &mut imgui::Context) {
        use imgui::MouseButton;

        let io = imgui.io_mut();

        // Push mouse down and up events in same frame
        io.add_mouse_button_event(MouseButton::Left, true);
        io.add_mouse_button_event(MouseButton::Left, false);

        debug_log!("[ignite_overlay] Simulated mouse click");
    }

    fn is_click_in_header(ui: &imgui::Ui, header_height: f32) -> bool {
        let io = ui.io();
        if !io.mouse_down[0] { // only left click
            return false;
        }
        let mouse_pos = io.mouse_pos;
        let win_pos = ui.window_pos();
        let win_size = ui.window_size();

        // Check if mouse is inside the window
        let inside_x = mouse_pos[0] >= win_pos[0] && mouse_pos[0] <= win_pos[0] + win_size[0];
        let inside_y = mouse_pos[1] >= win_pos[1] && mouse_pos[1] <= win_pos[1] + header_height;
        inside_x && inside_y
    }


    fn collect_boss_flags(&self) -> Vec<i32> {
        self.boss_regions
            .as_ref()
            .map(|regions| {
                regions.iter()
                    .flat_map(|r| r.bosses.iter().map(|b| b.flag_id))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }
}

//
// ----------------------------------------------------
// ImGui Render Loop Implementation
// ----------------------------------------------------
//

impl ImguiRenderLoop for EROverlayUi {
    fn initialize(&mut self, imgui: &mut imgui::Context, ctx: &mut dyn hudhook::RenderContext) {
        #[cfg(debug_assertions)]
        attach_console();

        debug_log!("[ignite_overlay] Initializing overlay...");

        if let Some(cfg) = &self.config {
            apply_style_config(imgui, cfg);
            apply_common_config(imgui, cfg, ctx);
        }

        // Collect the boss flags you want to monitor
        let flag_ids = self.collect_boss_flags();
        debug_log!("[ignite_overlay] Loaded {} boss flags", flag_ids.len());

        // Messmer's Kindling Shard
        let key_item_id = 2008021;

        // Kill reporting, if configured. Returns `None` when it is not, and the
        // monitor then behaves exactly as it did before this feature existed.
        let ingest_tx = start_reporter(
            self.ingest_settings.clone(),
            self.ingest_status.clone(),
            self.monitor_stop.clone(),
        );

        // Kick off the background game monitor
        start_game_monitor(
            self.state.clone(),
            self.igt.clone(),
            flag_ids,
            key_item_id,
            100,
            self.monitor_stop.clone(),
            ingest_tx,
        );

        debug_log!("[ignite_overlay] Game monitor started successfully.");
    }

    fn before_render(&mut self, imgui: &mut imgui::Context, _ctx: &mut dyn hudhook::RenderContext) {
        let io = imgui.io();
        let now = std::time::Instant::now();

        // --- Click Action ---
        if let Some(keys) = &self.click_action_keys {
            if keys.iter().all(|&k| io.keys_down[k as usize]) {
                if now.duration_since(self.last_click_time) > std::time::Duration::from_millis(200) {
                    Self::simulate_mouse_click(imgui);
                    self.last_click_time = now;
                }
            }
        }
    }

    fn render(&mut self, ui: &mut imgui::Ui) {
        // toggle key
        if let Some(keys) = self.toggle_full_mode_keys.as_ref() {
            if keys.iter().all(|&k| ui.is_key_pressed(k)) {
                self.full_mode = !self.full_mode;
                debug_log!("[ignite_overlay] full_mode toggled -> {}", self.full_mode);
            }
        }

        let io = ui.io();
        let (screen_w, screen_h) = (io.display_size[0], io.display_size[1]);

        // config rect for OPEN mode
        let (cfg_w, cfg_h, cfg_x, cfg_y) = {
            let (w_ratio, h_ratio) = self.config_dim();
            let [x_off, y_off] = self.config
                .as_ref().and_then(|c| c.style.as_ref()).and_then(|s| s.panel_pos)
                .unwrap_or(DEFAULT_PANEL_POS);

            let w = screen_w * w_ratio;
            let h = screen_h * h_ratio;

            let x = if x_off < 0.0 { screen_w - w + x_off } else { x_off };
            let y = if y_off < 0.0 { screen_h - h + y_off } else { y_off };
            (w, h, x, y)
        };

        if self.full_mode {
            // OPEN: fixed size from config
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
                        ui.text("⚠ Failed to load config:");
                        ui.text_wrapped(err);
                        return;
                    }
                    self.render_open(ui);
                });
        } else {
            // CLOSED: measure content and anchor to right/bottom correctly
            let [x_off, y_off] = self.config
                .as_ref().and_then(|c| c.style.as_ref()).and_then(|s| s.panel_pos)
                .unwrap_or(DEFAULT_PANEL_POS);

            let (closed_w, closed_h) = self.measure_closed_size(ui);

            let x = if x_off < 0.0 { screen_w - closed_w + x_off } else { x_off };
            let y = if y_off < 0.0 { screen_h - closed_h + y_off } else { y_off };

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
                        ui.text("⚠ Failed to load config:");
                        ui.text_wrapped(err);
                        return;
                    }
                    self.render_closed(ui);
                });
        }
    }

}

//
// ----------------------------------------------------
// EROverlayUi Clean Exit (stops Event Monitoring)
// ----------------------------------------------------
//

impl Drop for EROverlayUi {
    fn drop(&mut self) {
        // // Signal the monitoring thread to stop
        self.monitor_stop.store(true, std::sync::atomic::Ordering::SeqCst);

        // Wait a short moment for it to exit gracefully
        // (not strictly needed, since it's detached, but nice to ensure clean exit)
        std::thread::sleep(std::time::Duration::from_millis(150));

        debug_log!("[ignite_overlay] 🔻 Teardown: monitor thread stop signal sent.");

        debug_log!("[ignite_overlay] 🔻 Teardown: monitor stop signal sent.");
    }
}