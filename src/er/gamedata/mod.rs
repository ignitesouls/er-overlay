use crate::debug_log;
use crate::er::{get_text_section, parse_pattern, read_i32, read_ptr, scan_pattern};
use std::sync::OnceLock;

//
// ----------------------------------------------------
// GameDataMan Resolution
// ----------------------------------------------------
//

pub const GAME_DATA_TIME_OFFSET: usize = 0xA0;
pub const GAME_DATA_DEATHS_OFFSET: usize = 0x94;

pub unsafe fn find_gamedataman_ptr() -> Option<*const *const u8> {
    static GAME_DATA_MAN_GLOBAL: OnceLock<usize> = OnceLock::new();

    if let Some(ptr_addr) = GAME_DATA_MAN_GLOBAL.get() {
        return Some(*ptr_addr as *const *const u8);
    }

    let (base, size) = unsafe { get_text_section()? };
    let pattern = parse_pattern("48 8B 05 ?? ?? ?? ?? 48 85 C0 74 05 48 8B 40 58 C3 C3");
    let match_addr = unsafe { scan_pattern(base, size, &pattern)? };

    let disp = unsafe { read_i32(match_addr.add(3), "GameDataMan displacement")? };
    let rip_next = unsafe { match_addr.add(7) };
    let ptr_addr = unsafe { rip_next.offset(disp as isize) } as usize;

    let _ = GAME_DATA_MAN_GLOBAL.set(ptr_addr);
    debug_log!(
        "[ignite_overlay] Resolved GameDataMan global @ 0x{:x}",
        ptr_addr
    );

    Some(ptr_addr as *const *const u8)
}

/// Try to resolve GameDataMan pointer directly, matching the main branch monitor path.
pub unsafe fn try_resolve_gamedataman() -> Option<*const u8> {
    let man_ptr = find_gamedataman_ptr()?;
    let man = read_ptr(man_ptr as *const u8, "GameDataMan**")?;
    if man.is_null() {
        return None;
    }

    Some(man)
}

/// Reads the in-game time in milliseconds (IGT)
pub unsafe fn read_in_game_time(game_data_man: *const u8) -> Option<u32> {
    let val = read_i32(game_data_man.add(GAME_DATA_TIME_OFFSET), "IGT")?;
    Some(val as u32)
}

/// Reads the number of times player has died
pub unsafe fn read_death_count(game_data_man: *const u8) -> Option<u32> {
    let val = read_i32(game_data_man.add(GAME_DATA_DEATHS_OFFSET), "Deaths")?;
    Some(val as u32)
}
