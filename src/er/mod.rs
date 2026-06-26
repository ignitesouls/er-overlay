#![allow(unsafe_op_in_unsafe_fn)]

pub mod events;
pub mod gamedata;
pub mod grace;
pub mod inventory;
pub mod item_spawn;

use crate::debug_log;
use std::{mem, ptr, slice};
use winapi::um::winnt::IMAGE_NT_HEADERS64;
use windows_sys::Win32::System::Diagnostics::Debug::IMAGE_SECTION_HEADER;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;
use windows_sys::Win32::System::SystemServices::IMAGE_DOS_HEADER;

//
// ----------------------------------------------------
// Low-Level Safe Reads
// ----------------------------------------------------
//

#[inline(always)]
pub unsafe fn read_typed<T: Copy + Default>(addr: *const u8, label: &str) -> Option<T> {
    if addr.is_null() {
        debug_log!("[ignite_overlay] âŒ null pointer @ {label}");
        return None;
    }
    let align = mem::align_of::<T>();
    let size = mem::size_of::<T>();

    if (addr as usize) % align != 0 {
        debug_log!(
            "[ignite_overlay] âš  misaligned read({label}) @ 0x{:x}, copying manually",
            addr as usize
        );
        let mut tmp = T::default();
        ptr::copy_nonoverlapping(addr, &mut tmp as *mut _ as *mut u8, size);
        return Some(tmp);
    }

    Some(ptr::read_unaligned(addr as *const T))
}

#[inline(always)]
pub unsafe fn read_u8(addr: *const u8, label: &str) -> Option<u8> {
    read_typed(addr, label)
}
#[inline(always)]
pub unsafe fn read_i32(addr: *const u8, label: &str) -> Option<i32> {
    read_typed(addr, label)
}
#[inline(always)]
pub unsafe fn read_u64(addr: *const u8, label: &str) -> Option<u64> {
    read_typed(addr, label)
}
#[inline(always)]
pub unsafe fn read_ptr(addr: *const u8, label: &str) -> Option<*const u8> {
    read_typed(addr, label)
}

//
// ----------------------------------------------------
// Pattern Scanning
// ----------------------------------------------------
//

#[derive(Clone, Copy)]
enum PatternByte {
    Exact(u8),
    Wildcard,
}

fn parse_pattern(pat: &str) -> Vec<PatternByte> {
    pat.split_whitespace()
        .map(|b| {
            if b == "??" || b == "?" {
                PatternByte::Wildcard
            } else {
                PatternByte::Exact(u8::from_str_radix(b, 16).unwrap())
            }
        })
        .collect()
}

unsafe fn get_text_section() -> Option<(*const u8, usize)> {
    let hmod = GetModuleHandleA(ptr::null());
    if hmod.is_null() {
        return None;
    }

    let base = hmod as *const u8;
    let dos = &*(base as *const IMAGE_DOS_HEADER);
    let nt = &*((base.add(dos.e_lfanew as usize)) as *const IMAGE_NT_HEADERS64);

    let sections = (nt as *const _ as *const u8).add(mem::size_of::<IMAGE_NT_HEADERS64>())
        as *const IMAGE_SECTION_HEADER;

    let num_sections = (*nt).FileHeader.NumberOfSections as usize;
    (0..num_sections).find_map(|i| {
        let sect = &*sections.add(i);
        if &sect.Name[..5] == b".text" {
            Some((
                base.add(sect.VirtualAddress as usize),
                sect.Misc.VirtualSize as usize,
            ))
        } else {
            None
        }
    })
}

unsafe fn scan_pattern(base: *const u8, size: usize, pattern: &[PatternByte]) -> Option<*const u8> {
    let bytes = slice::from_raw_parts(base, size);
    let pat_len = pattern.len();

    'outer: for i in 0..=size - pat_len {
        for j in 0..pat_len {
            if let PatternByte::Exact(b) = pattern[j] {
                if bytes[i + j] != b {
                    continue 'outer;
                }
            }
        }
        return Some(base.add(i));
    }
    None
}
