use crate::debug_log;
use crate::er::{parse_pattern, scan_pattern, read_i32, read_ptr, get_text_section};

//
// ----------------------------------------------------
// GameDataMan Resolution
// ----------------------------------------------------
//

pub const GAME_DATA_TIME_OFFSET: usize = 0xA0;
pub const GAME_DATA_DEATHS_OFFSET: usize = 0x94;

pub unsafe fn find_gamedataman_ptr() -> Option<*const *const u8> {
    use std::slice;

    let (base, size) = get_text_section()?;
    let pattern = parse_pattern("48 8B 05 ?? ?? ?? ?? 48 85 C0 74 05 48 8B 40 58 C3 C3");
    let match_addr = scan_pattern(base, size, &pattern)?;
    debug_log!(
        "[ignite_overlay] Found GameDataMan pattern @ 0x{:x}",
        match_addr as usize
    );

    // Hex dump for reference
    let dump = slice::from_raw_parts(match_addr, 24);
    let mut line = String::new();
    for (i, b) in dump.iter().enumerate() {
        use std::fmt::Write;
        let _ = write!(&mut line, "{:02X} ", b);
        if (i + 1) % 16 == 0 {
            debug_log!("[ignite_overlay]    {}", line);
            line.clear();
        }
    }

    // Displacement at +3 (RIP-relative)
    let disp = read_i32(match_addr.add(3), "GameDataMan displacement")?;
    let rip_next = match_addr.add(7);
    let ptr_addr = rip_next.offset(disp as isize);

    debug_log!(
        "[ignite_overlay] ➜ computed GameDataMan @ 0x{:x}",
        ptr_addr as usize
    );

    Some(ptr_addr as *const *const u8)
}

/// Try to resolve GameDataMan pointer directly
pub unsafe fn try_resolve_gamedataman() -> Option<*const u8> {
    let man_ptr = find_gamedataman_ptr()?;
    let man = read_ptr(man_ptr as *const u8, "GameDataMan**")?;
    if man.is_null() {
        debug_log!("[ignite_overlay] GameDataMan pointer null");
        return None;
    }

    Some(man)
}

/// Reads the in-game time in milliseconds (IGT)
pub unsafe fn read_in_game_time(game_flag_man: *const u8) -> Option<u32> {
    let val = read_i32(game_flag_man.add(GAME_DATA_TIME_OFFSET), "IGT")?;
    Some(val as u32)
}

/// Reads the number of times player has died
pub unsafe fn read_death_count(game_flag_man: *const u8) -> Option<u32> {
    let val = read_i32(game_flag_man.add(GAME_DATA_DEATHS_OFFSET), "Deaths")?;
    Some(val as u32)
}

