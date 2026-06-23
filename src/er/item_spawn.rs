use std::{
    collections::VecDeque,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use eldenring::{
    cs::{CSTaskGroupIndex, CSTaskImp},
    fd4::FD4TaskData,
};
use fromsoftware_shared::{singleton::get_instance, task::SharedTaskImpExt};

use crate::{
    debug_log,
    er::{
        get_text_section, inventory::equip_weapon_right_hand_primary, parse_pattern, scan_pattern,
    },
};

const WEAPON_CATEGORY_BITS: u32 = 0x0000_0000;

type GiveInventoryItemFn = unsafe extern "C" fn(u32, i32, i32);

#[derive(Debug)]
struct GrantRequest {
    category_bits: u32,
    item_id: i32,
    quantity: i32,
}

static GRANT_QUEUE: OnceLock<Mutex<VecDeque<GrantRequest>>> = OnceLock::new();
static SPAWN_SERVICE_STARTED: AtomicBool = AtomicBool::new(false);

fn grant_queue() -> &'static Mutex<VecDeque<GrantRequest>> {
    GRANT_QUEUE.get_or_init(|| Mutex::new(VecDeque::new()))
}

unsafe fn resolve_give_inventory_item() -> Option<GiveInventoryItemFn> {
    let (base, size) = unsafe { get_text_section()? };
    let pattern = parse_pattern(
        "40 56 57 41 56 48 83 EC 50 48 C7 44 24 30 FE FF FF FF \
         48 89 5C 24 70 48 89 6C 24 78 41 8B F8 8B F2 \
         48 8B 05 ?? ?? ?? ?? 48 8B 58 08",
    );

    let match_addr = unsafe { scan_pattern(base, size, &pattern)? };
    debug_log!(
        "[ignite_overlay] Found give inventory item routine @ 0x{:x}",
        match_addr as usize
    );

    Some(unsafe { std::mem::transmute::<*const u8, GiveInventoryItemFn>(match_addr) })
}

pub fn start_item_spawn_service() {
    if SPAWN_SERVICE_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }

    thread::spawn(move || {
        debug_log!("[ignite_overlay] Experimental item spawn service enabled");

        let cs_task = loop {
            if let Some(cs_task) = unsafe { get_instance::<CSTaskImp>() } {
                break cs_task;
            }

            debug_log!("[ignite_overlay] Waiting for CSTaskImp before item spawn service...");
            thread::sleep(Duration::from_secs(1));
        };

        let Some(give_inventory_item) = (unsafe { resolve_give_inventory_item() }) else {
            debug_log!("[ignite_overlay] Item spawn aborted: grant routine signature not found");
            return;
        };

        let handle = cs_task.run_recurring(
            move |_: &FD4TaskData| {
                let Some(request) = grant_queue().lock().ok().and_then(|mut q| q.pop_front())
                else {
                    return;
                };

                unsafe {
                    give_inventory_item(
                        request.category_bits,
                        request.item_id,
                        request.quantity,
                    );
                }

                debug_log!(
                    "[ignite_overlay] Requested item grant: category=0x{:08x}, item_id={}, quantity={}",
                    request.category_bits,
                    request.item_id,
                    request.quantity
                );

                if request.category_bits == WEAPON_CATEGORY_BITS {
                    let _ = equip_weapon_right_hand_primary(request.item_id);
                }
            },
            CSTaskGroupIndex::FrameBegin,
        );

        std::mem::forget(handle);
    });
}

pub fn request_weapon_grant(weapon_id: i32, quantity: i32) {
    start_item_spawn_service();

    let request = GrantRequest {
        category_bits: WEAPON_CATEGORY_BITS,
        item_id: weapon_id,
        quantity,
    };

    match grant_queue().lock() {
        Ok(mut queue) => {
            queue.push_back(request);
            debug_log!(
                "[ignite_overlay] Queued weapon grant: weapon_id={}, quantity={}",
                weapon_id,
                quantity
            );
        }
        Err(err) => {
            debug_log!(
                "[ignite_overlay] Failed to queue weapon grant weapon_id={}: {:?}",
                weapon_id,
                err
            );
        }
    }
}

pub fn start_test_weapon_grant(weapon_id: i32) {
    debug_log!(
        "[ignite_overlay] Experimental item spawn enabled for weapon_id={}",
        weapon_id
    );
    request_weapon_grant(weapon_id, 1);
}
