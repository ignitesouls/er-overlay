use std::{collections::HashMap};
use fromsoftware_shared::singleton::get_instance;
use eldenring::cs::{CSEventFlagMan, EventFlag};

// Using Vswarte's crate. Traverses the event tree for every flag read (can't cache because struct has private fields)
#[inline(always)]
pub fn get_event_flags(flag_ids: &[u32]) -> Option<HashMap<u32, bool>> {
    let flagman = unsafe { get_instance::<CSEventFlagMan>()? };
    let vmf = &flagman.virtual_memory_flag;

    Some(
        flag_ids
            .iter()
            .map(|&id| {
                let flag = EventFlag::from(id);
                (id, vmf.get_flag(flag))
            })
            .collect(),
    )
}

use std::{ptr, slice};
use crate::debug_log;
use crate::er::{parse_pattern, scan_pattern, read_u8, read_u64, read_i32, read_ptr, get_text_section};

//
// ----------------------------------------------------
// Event Flag Core Constants and Structs
// ----------------------------------------------------
//

const EVENT_FLAG_DIVISOR: usize = 0x1C;
pub const FLAG_HOLDER_ENTRY_SIZE: usize = 0x20;
const FLAG_HOLDER: usize = 0x28;
pub const FLAG_GROUP_ROOT_NODE: usize = 0x38;

const NODE_LEFT: usize = 0x0;
const NODE_PARENT: usize = 0x8;
const NODE_RIGHT: usize = 0x10;
const NODE_IS_LEAF: usize = 0x19;
const NODE_GROUP: usize = 0x20;
const NODE_LOCATION_MODE: usize = 0x28;
const NODE_LOCATION: usize = 0x30;

#[repr(C)]
pub struct EventFlagGroupNode {
    pub left: *const EventFlagGroupNode,
    pub parent: *const EventFlagGroupNode,
    pub right: *const EventFlagGroupNode,
    pub _pad1: [u8; 0x19 - 0x18],
    pub is_leaf: u8,
    pub _pad2: [u8; 0x20 - 0x1A],
    pub group: i32,
    pub location_mode: i32,
    pub location: i64,
}

#[derive(Clone, Copy)]
pub struct FlagLocation {
    pub base: *const u8,
    pub bit:  i32,
}

//
// ----------------------------------------------------
// EventFlagMan resolution
// ----------------------------------------------------
//

pub unsafe fn find_event_flag_manager_ptr() -> Option<*const *const u8> {
    let (base, size) = get_text_section()?;
    let pattern = parse_pattern(
        "48 83 3D ?? ?? ?? ?? 00 0F 84 ?? ?? 00 00 44 8B E6 85 C0 0F 84 ?? ?? 00 00"
    );
    let match_addr = scan_pattern(base, size, &pattern)?;
    debug_log!("[ignite_overlay] Found GLOBAL_CSEventFlagMan pattern at 0x{:x}", match_addr as usize);

    // Show hex dump for clarity
    let dump = slice::from_raw_parts(match_addr, 32);
    let mut line = String::new();
    for (i, b) in dump.iter().enumerate() {
        use std::fmt::Write;
        let _ = write!(&mut line, "{:02X} ", b);
        if (i + 1) % 16 == 0 { debug_log!("[ignite_overlay]    {}", line); line.clear(); }
    }

    // Displacement at +3 (RIP-relative LEA)
    let disp = read_i32(match_addr.add(3), "GLOBAL_CSEventFlagMan displacement")?;
    let rip_next = match_addr.add(8);
    let ptr_addr = rip_next.offset(disp as isize);

    debug_log!("[ignite_overlay] ➜ computed GLOBAL_CSEventFlagMan @ 0x{:x}", ptr_addr as usize);

    // Ensure 8-byte alignment
    Some(if (ptr_addr as usize) % 8 == 0 {
        ptr_addr as *const *const u8
    } else {
        let aligned = (ptr_addr as usize + 7) & !7;
        aligned as *const *const u8
    })
}

/// Try to resolve EventFlagMan pointer directly
pub unsafe fn try_resolve_eventflagman() -> Option<*const u8> {
    let man_ptr = find_event_flag_manager_ptr()?;
    let man = read_ptr(man_ptr as *const u8, "EventFlagMan**")?;
    if man.is_null() { return None; }

    let divisor = read_i32(man.add(EVENT_FLAG_DIVISOR), "divisor")?;
    let entry   = read_i32(man.add(FLAG_HOLDER_ENTRY_SIZE), "entry_size")?;
    if divisor <= 0 || entry <= 0 { return None; }

    Some(man)
}

//
// ----------------------------------------------------
// Higher-level helpers (tree and accessors)
// ----------------------------------------------------
//

#[inline(always)]
pub unsafe fn read_root(evt_flag_man: *const u8) -> *const u8 {
    read_ptr(evt_flag_man.add(FLAG_GROUP_ROOT_NODE), "FLAG_GROUP_ROOT_NODE").unwrap_or(ptr::null())
}

#[inline(always)]
pub unsafe fn read_entry_size(evt_flag_man: *const u8) -> usize {
    read_i32(evt_flag_man.add(FLAG_HOLDER_ENTRY_SIZE), "FLAG_HOLDER_ENTRY_SIZE")
        .unwrap_or_default() as usize
}

//
// ----------------------------------------------------
// Flag Access Logic
// ----------------------------------------------------
//

pub unsafe fn get_event_flag_loc_and_bit(
    evt_flag_man: *const u8,
    flag: i32,
) -> Option<(*const u8, i32)> {
    if evt_flag_man.is_null() {
        return None;
    }

    let divisor = read_i32(evt_flag_man.add(EVENT_FLAG_DIVISOR), "divisor")?;
    let entry_size = read_i32(evt_flag_man.add(FLAG_HOLDER_ENTRY_SIZE), "entry_size")?;
    if divisor == 0 || entry_size == 0 {
        return None;
    }

    let group_num = flag / divisor;
    let bit_num_full = flag % divisor;

    let root =
        read_ptr(evt_flag_man.add(FLAG_GROUP_ROOT_NODE), "root")? as *const EventFlagGroupNode;
    if root.is_null() {
        return None;
    }

    let mut current =
        read_ptr(root.cast::<u8>().add(NODE_PARENT), "NODE_PARENT")? as *const EventFlagGroupNode;
    let mut found = root;

    let mut walk_count = 0;
    loop {
        let is_leaf = read_u8(current.cast::<u8>().add(NODE_IS_LEAF), "is_leaf")?;
        if is_leaf != 0 {
            break;
        }

        walk_count += 1;
        if walk_count > 1000 {
            debug_log!("[event_flags] walk overflow for flag {flag}");
            return None;
        }

        let current_group = read_i32(current.cast::<u8>().add(NODE_GROUP), "current_group")?;
        let next: *const EventFlagGroupNode;

        if current_group < group_num {
            // C# logic: step back before moving right
            next = read_ptr(current.cast::<u8>().add(NODE_RIGHT), "NODE_RIGHT")?
                as *const EventFlagGroupNode;
            current = found;
        } else {
            next = read_ptr(current.cast::<u8>().add(NODE_LEFT), "NODE_LEFT")?
                as *const EventFlagGroupNode;
        }

        found = current;
        current = next;

        if current.is_null() {
            debug_log!("[event_flags] null next in tree walk for flag {flag}");
            return None;
        }
    }

    if found == root || group_num < read_i32(found.cast::<u8>().add(NODE_GROUP), "group")? {
        debug_log!("[event_flags] failed: bad group compare for {flag}");
        return None;
    }

    let loc_mode = read_i32(found.cast::<u8>().add(NODE_LOCATION_MODE), "loc_mode")?;

    let ptr = match loc_mode {
        2 => read_u64(found.cast::<u8>().add(NODE_LOCATION), "NODE_LOCATION")? as *const u8,
        1 => {
            let holder = read_ptr(evt_flag_man.add(FLAG_HOLDER), "FLAG_HOLDER")?;
            let loc = read_i32(found.cast::<u8>().add(NODE_LOCATION), "NODE_LOCATION")?;
            holder.add(loc as usize * entry_size as usize)
        }
        _ => {
            debug_log!("[event_flags] unknown loc_mode={loc_mode} for {flag}");
            return None;
        }
    };

    Some((ptr, bit_num_full))
}

/// Builds a cache of flag_id → (holder base, bit offset)
pub unsafe fn build_cache(evt_flag_man: *const u8, flag_ids: &Vec<i32>) -> HashMap<i32, FlagLocation> {
    let mut cache = HashMap::with_capacity(flag_ids.len());
    for &id in flag_ids {
        if let Some((base, bit)) = get_event_flag_loc_and_bit(evt_flag_man, id) {
            cache.insert(id, FlagLocation { base, bit });
        }
    }
    cache
}

/// Reads flag state directly from a flag's location in memory
pub unsafe fn read_from_flag_location(loc: &FlagLocation) -> bool {
    let byte_ix = (loc.bit / 8) as usize;
    let bit_mask = 1u8 << (7 - (loc.bit % 8));

    let byte_val = read_u8(loc.base.add(byte_ix), "flag_byte")
        .unwrap_or_default();

    (byte_val & bit_mask) != 0
}
