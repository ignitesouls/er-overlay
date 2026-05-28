use std::fs;
use imgui::{Key, StyleColor, Context, FontConfig, FontSource};
use serde::Deserialize;
use crate::overlay::embedded_font::EMBEDDED_FONT;
use crate::util::introspection::{get_dll_directory};
use crate::debug_log;

pub const DEFAULT_PANEL_DIM: [f32; 2] = [0.17, 0.92];
pub const DEFAULT_PANEL_POS: [f32; 2] = [-10.0, 10.0];
pub const DEFAULT_DISPLAY_TEXT: &str = "Current: {kills}/{total}$nIGT: {igt}$nDeaths: {deaths}";


#[derive(Debug, Deserialize)]
pub struct Common {
    pub console: Option<bool>,
    pub font: Option<String>,
    pub font_size: Option<f32>,
    pub charset: Option<String>,
    pub language: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Input {
    pub unload: Option<String>,
    pub toggle_full_mode: Option<String>,
    pub click_action: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Style {
    pub text_color: Option<[u8; 4]>,
    pub check_mark_color: Option<[u8; 4]>,
    pub bg_color: Option<[u8; 4]>,
    pub border_color: Option<[u8; 4]>,
    pub button_color: Option<[u8; 4]>,
    pub button_hover_color: Option<[u8; 4]>,
    pub button_press_color: Option<[u8; 4]>,
    pub node_color: Option<[u8; 4]>,
    pub node_hover_color: Option<[u8; 4]>,
    pub node_press_color: Option<[u8; 4]>,
    pub scroll_bg_color: Option<[u8; 4]>,
    pub scroll_color: Option<[u8; 4]>,
    pub scroll_hover_color: Option<[u8; 4]>,
    pub scroll_press_color: Option<[u8; 4]>,
    pub border_width: Option<f32>,
    pub rounding: Option<f32>,
    pub panel_pos: Option<[f32; 2]>,
    pub panel_dim: Option<[f32; 2]>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Boss {
    pub data_file: Option<String>,
    pub schedule_file: Option<String>,
    pub seed: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Overlay {
    pub display_text: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum TimerMode {
    Regular,
    Timer,
    Prep,
    PrepTimer,
}

#[derive(Debug, Deserialize)]
pub struct TimerConfig {
    pub mode: TimerMode,
    pub prep_minutes: Option<u32>,
    pub timer_minutes: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct IgniteConfig {
    pub common: Option<Common>,
    pub input: Option<Input>,
    pub style: Option<Style>,
    pub boss: Option<Boss>,
    pub overlay: Option<Overlay>,
    pub timer: Option<TimerConfig>,
}

pub fn read_config() -> Result<IgniteConfig, String> {
    let dll_dir = get_dll_directory()
        .ok_or("Failed to get DLL directory".to_string())?;
    let config_path = dll_dir.join("ignite_overlay_config.toml");

    let contents = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("Failed to read {:?}: {}", config_path, e))?;

    toml::from_str::<IgniteConfig>(&contents)
        .map_err(|e| format!("Failed to parse {:?}: {}", config_path, e))
}

pub fn apply_style_config(imgui: &mut Context, cfg: &IgniteConfig) {
    if let Some(style_cfg) = cfg.style.as_ref() {
        let style = imgui.style_mut();

        macro_rules! color {
            ($field:ident, $col:expr) => {
                if let Some(c) = style_cfg.$field {
                    style.colors[$col as usize] = rgba_u8_to_f32(c);
                }
            };
        }

        // --- text and basics ---
        color!(text_color, StyleColor::Text);
        color!(check_mark_color, StyleColor::CheckMark);
        color!(bg_color, StyleColor::WindowBg);
        color!(border_color, StyleColor::Border);

        // --- buttons ---
        color!(button_color, StyleColor::Button);
        color!(button_hover_color, StyleColor::ButtonHovered);
        color!(button_press_color, StyleColor::ButtonActive);

        // --- nodes (we’ll map to tree headers for best visual equivalence) ---
        color!(node_color, StyleColor::Header);
        color!(node_hover_color, StyleColor::HeaderHovered);
        color!(node_press_color, StyleColor::HeaderActive);

        // --- scrollbars ---
        color!(scroll_bg_color, StyleColor::ScrollbarBg);
        color!(scroll_color, StyleColor::ScrollbarGrab);
        color!(scroll_hover_color, StyleColor::ScrollbarGrabHovered);
        color!(scroll_press_color, StyleColor::ScrollbarGrabActive);

        // --- rounding & borders ---
        if let Some(rounding) = style_cfg.rounding {
            style.window_rounding = rounding;
            style.frame_rounding = rounding;
            style.scrollbar_rounding = rounding;
        }

        if let Some(border_width) = style_cfg.border_width {
            style.window_border_size = border_width;
            style.frame_border_size = border_width;
        }
    }
}

fn rgba_u8_to_f32(c: [u8; 4]) -> [f32; 4] {
    [
        c[0] as f32 / 255.0,
        c[1] as f32 / 255.0,
        c[2] as f32 / 255.0,
        c[3] as f32 / 255.0,
    ]
}


/// Apply common config (font, size, charset, etc.)
pub fn apply_common_config(
    imgui: &mut Context,
    cfg: &IgniteConfig,
    ctx: &mut dyn hudhook::RenderContext,
) {
    let font_size = cfg
        .common
        .as_ref()
        .and_then(|c| c.font_size)
        .unwrap_or(20.0) as f32;

    // --- Step 1: Build the font atlas ---
    let (rgba, width, height) = {
        let fonts = imgui.fonts();
        fonts.clear();

        let mut font_loaded = false;
        let mut font_used = String::new();

        // --- 1️⃣ Charset-based font loading ---
        if let Some(common) = &cfg.common {
            if let Some(charset) = &common.charset {
                let charset = charset.to_lowercase();
                let font_path_opt = match charset.as_str() {
                    "jajp" => Some("C:\\Windows\\Fonts\\meiryo.ttc"),      // Japanese
                    "kokr" => Some("C:\\Windows\\Fonts\\malgun.ttf"),      // Korean
                    "zhcn" => Some("C:\\Windows\\Fonts\\msyh.ttc"),        // Simplified Chinese
                    "zhtw" => Some("C:\\Windows\\Fonts\\mingliu.ttc"),     // Traditional Chinese
                    "thth" => Some("C:\\Windows\\Fonts\\tahoma.ttf"),      // Thai fallback
                    "ruru" => Some("C:\\Windows\\Fonts\\arial.ttf"),       // Cyrillic
                    "plpl" => Some("C:\\Windows\\Fonts\\arial.ttf"),       // Latin extended
                    _ => None,
                };

                if let Some(path) = font_path_opt {
                    if let Ok(bytes) = fs::read(path) {
                        let leaked = Box::leak(bytes.into_boxed_slice());
                        fonts.add_font(&[FontSource::TtfData {
                            data: leaked,
                            size_pixels: font_size,
                            config: Some(FontConfig::default()),
                        }]);
                        font_loaded = true;
                        font_used = format!("system charset font: {}", charset);
                        debug_log!("[ignite_overlay] ✅ Loaded charset font '{}'", charset);
                    } else {
                        debug_log!("[ignite_overlay] ⚠ Charset font '{}' not found at {}", charset, path);
                    }
                }
            }
        }

        // --- 2️⃣ User-defined font path ---
        if !font_loaded {
            if let Some(path) = cfg
                .common
                .as_ref()
                .and_then(|c| c.font.as_ref())
                .filter(|p| !p.is_empty())
            {
                if let Ok(bytes) = fs::read(path) {
                    let leaked = Box::leak(bytes.into_boxed_slice());
                    fonts.add_font(&[FontSource::TtfData {
                        data: leaked,
                        size_pixels: font_size,
                        config: Some(FontConfig::default()),
                    }]);
                    font_loaded = true;
                    font_used = path.to_string();
                    debug_log!("[ignite_overlay] ✅ Loaded user font '{}'", path);
                } else {
                    debug_log!("[ignite_overlay] ⚠ Could not read font '{}'", path);
                }
            }
        }

        // --- 3️⃣ Embedded fallback font ---
        if !font_loaded {
            fonts.add_font(&[FontSource::TtfData {
                data: EMBEDDED_FONT,
                size_pixels: font_size,
                config: Some(FontConfig::default()),
            }]);
            font_used = "EmbeddedFont".to_string();
            font_loaded = true;
            debug_log!("[ignite_overlay] ✅ Loaded embedded font");
        }

        // --- 4️⃣ Final fallback: ImGui default font ---
        if !font_loaded {
            fonts.add_font(&[FontSource::DefaultFontData {
                config: Some(FontConfig {
                    size_pixels: font_size,
                    ..Default::default()
                }),
            }]);
            font_used = "ImGui Default".to_string();
            debug_log!("[ignite_overlay] ⚠ Fallback to ImGui default font");
        }

        // Build atlas & fix discoloration artifacts
        let atlas = fonts.build_rgba32_texture();
        let mut rgba = atlas.data.to_vec();
        for px in rgba.chunks_exact_mut(4) {
            if px[3] == 0 {
                px[0] = 0;
                px[1] = 0;
                px[2] = 0;
            }
        }

        debug_log!(
            "[ignite_overlay] 🧩 Font loaded: '{}' ({}x{})",
            font_used,
            atlas.width,
            atlas.height
        );

        (rgba, atlas.width, atlas.height)
    };

    // --- Step 2: Upload texture to hudhook ---
    match ctx.load_texture(&rgba, width, height) {
        Ok(tex_id) => {
            let fonts = imgui.fonts();
            fonts.tex_id = tex_id;
            fonts.clear_tex_data();
            debug_log!(
                "[ignite_overlay] ✅ Uploaded font atlas {}x{} tex_id={:?}",
                width,
                height,
                tex_id
            );
        }
        Err(e) => debug_log!("[ignite_overlay] ❌ Failed to upload font texture: {e:?}"),
    };

    drop(rgba);
}

pub fn key_from_name(name: &str) -> Option<Key> {
    use Key::*;
    match name.trim().to_uppercase().as_str() {
        // --- Modifier keys ---
        "LCTRL" | "CTRL" => Some(LeftCtrl),
        "RCTRL" => Some(RightCtrl),
        "LSHIFT" | "SHIFT" => Some(LeftShift),
        "RSHIFT" => Some(RightShift),
        "LALT" | "ALT" => Some(LeftAlt),
        "RALT" => Some(RightAlt),
        "LWIN" | "WIN" => Some(LeftSuper),
        "RWIN" => Some(RightSuper),

        // --- Mouse buttons ---
        "LBUTTON" => Some(MouseLeft),
        "RBUTTON" => Some(MouseRight),
        "MBUTTON" => Some(MouseMiddle),
        "XBUTTON1" => Some(MouseX1),
        "XBUTTON2" => Some(MouseX2),

        // --- Control & navigation ---
        "BACK" | "BACKSPACE" => Some(Backspace),
        "TAB" => Some(Tab),
        "RETURN" | "ENTER" => Some(Enter),
        "ESCAPE" | "ESC" => Some(Escape),
        "SPACE" => Some(Space),
        "DELETE" => Some(Delete),
        "INSERT" => Some(Insert),
        "HOME" => Some(Home),
        "END" => Some(End),
        "PRIOR" | "PAGEUP" => Some(PageUp),
        "NEXT" | "PAGEDOWN" => Some(PageDown),
        "LEFT" => Some(LeftArrow),
        "RIGHT" => Some(RightArrow),
        "UP" => Some(UpArrow),
        "DOWN" => Some(DownArrow),

        // --- Function keys ---
        "F1" => Some(F1),
        "F2" => Some(F2),
        "F3" => Some(F3),
        "F4" => Some(F4),
        "F5" => Some(F5),
        "F6" => Some(F6),
        "F7" => Some(F7),
        "F8" => Some(F8),
        "F9" => Some(F9),
        "F10" => Some(F10),
        "F11" => Some(F11),
        "F12" => Some(F12),

        // --- Alphabet keys ---
        "A" => Some(A), "B" => Some(B), "C" => Some(C), "D" => Some(D),
        "E" => Some(E), "F" => Some(F), "G" => Some(G), "H" => Some(H),
        "I" => Some(I), "J" => Some(J), "K" => Some(K), "L" => Some(L),
        "M" => Some(M), "N" => Some(N), "O" => Some(O), "P" => Some(P),
        "Q" => Some(Q), "R" => Some(R), "S" => Some(S), "T" => Some(T),
        "U" => Some(U), "V" => Some(V), "W" => Some(W), "X" => Some(X),
        "Y" => Some(Y), "Z" => Some(Z),

        // --- Number row ---
        "0" => Some(Alpha0), "1" => Some(Alpha1), "2" => Some(Alpha2), "3" => Some(Alpha3),
        "4" => Some(Alpha4), "5" => Some(Alpha5), "6" => Some(Alpha6), "7" => Some(Alpha7),
        "8" => Some(Alpha8), "9" => Some(Alpha9),

        // --- Numpad ---
        "NUMPAD0" | "NUM0" => Some(Keypad0),
        "NUMPAD1" | "NUM1" => Some(Keypad1),
        "NUMPAD2" | "NUM2" => Some(Keypad2),
        "NUMPAD3" | "NUM3" => Some(Keypad3),
        "NUMPAD4" | "NUM4" => Some(Keypad4),
        "NUMPAD5" | "NUM5" => Some(Keypad5),
        "NUMPAD6" | "NUM6" => Some(Keypad6),
        "NUMPAD7" | "NUM7" => Some(Keypad7),
        "NUMPAD8" | "NUM8" => Some(Keypad8),
        "NUMPAD9" | "NUM9" => Some(Keypad9),
        "DIVIDE" | "/" => Some(KeypadDivide),
        "MULTIPLY" | "*" => Some(KeypadMultiply),
        "-" | "MINUS" | "HYPHEN" => Some(Key::Minus),
        "SUBTRACT" | "NUMPADSUBTRACT" => Some(Key::KeypadSubtract),
        "ADD" | "+" => Some(KeypadAdd),
        "EQUAL" | "=" => Some(Equal),
        "DECIMAL" | "NUMPADPERIOD" => Some(KeypadDecimal),

        // --- Lock keys ---
        "CAPSLOCK" | "CAPITAL" => Some(CapsLock),
        "NUMLOCK" => Some(NumLock),
        "SCROLL" | "SCROLLLOCK" => Some(ScrollLock),

        // --- Symbols ---
        "," => Some(Comma),
        "." | "PERIOD" => Some(Period),
        ";" => Some(Semicolon),
        "'" => Some(Apostrophe),
        "[" => Some(LeftBracket),
        "]" => Some(RightBracket),
        "\\" => Some(Backslash),
        "~" => Some(GraveAccent),

        // --- Misc / System ---
        "PRINT" | "PRINTSCREEN" | "SNAPSHOT" => Some(PrintScreen),
        "PAUSE" => Some(Pause),

        _ => None,
    }
}

/// Parse a single or multi-key combination like "CTRL+SHIFT+F1".
/// Returns a vector of `Key` values (empty if none matched).
pub fn parse_key_combo(combo: &str) -> Vec<Key> {
    combo
        .split('+')
        .filter_map(|part| key_from_name(part.trim()))
        .collect()
}