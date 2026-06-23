use crate::debug_log;
use eldenring::cs::{ChrAsmArmStyle, ChrAsmSlot, ItemCategory, ItemId, WorldChrMan};
use fromsoftware_shared::singleton::get_instance;

#[inline(always)]
pub fn get_key_item_quantity(key_item_id: i32) -> u32 {
    let Some(world_chr_man) = (unsafe { get_instance::<WorldChrMan>() }) else {
        return 0;
    };
    let Some(player) = &world_chr_man.main_player else {
        return 0;
    };

    let items = &player
        .player_game_data
        .equipment
        .equip_inventory_data
        .items_data;
    let item_id = ItemId::from_parts(key_item_id, ItemCategory::Goods);

    for entry in items.key_items() {
        if entry.item_id == item_id {
            return entry.quantity;
        }
    }
    0
}

pub fn equip_weapon_right_hand_primary(weapon_id: i32) -> bool {
    let Some(world_chr_man) = (unsafe { get_instance::<WorldChrMan>() }) else {
        debug_log!(
            "[ignite_overlay] Equip weapon aborted: WorldChrMan unavailable for weapon_id={}",
            weapon_id
        );
        return false;
    };

    let Some(player) = &mut world_chr_man.main_player else {
        debug_log!(
            "[ignite_overlay] Equip weapon aborted: main player unavailable for weapon_id={}",
            weapon_id
        );
        return false;
    };

    let equipment = &mut player.player_game_data.equipment;
    let item_id = ItemId::from_parts(weapon_id, ItemCategory::Weapon);

    let Some((inventory_index, gaitem_handle)) = equipment
        .equip_inventory_data
        .items_data
        .normal_items()
        .iter()
        .enumerate()
        .find(|(_, entry)| entry.item_id == item_id)
        .map(|(inventory_index, entry)| (inventory_index, entry.gaitem_handle))
    else {
        debug_log!(
            "[ignite_overlay] Equip weapon pending: weapon_id={} not found in normal inventory",
            weapon_id
        );
        return false;
    };

    let slot = ChrAsmSlot::WeaponRight1 as usize;
    equipment.equipment_entries.weapon_primary_right = item_id;
    equipment.equip_item_data.equip_entries.weapon_primary_right = item_id;
    equipment.chr_asm.gaitem_handles[slot] = gaitem_handle;
    equipment.chr_asm.equipment_param_ids[slot] = weapon_id;
    equipment.chr_asm.equipment.selected_slots.right_weapon_slot = 0;
    equipment.chr_asm.equipment.arm_style = ChrAsmArmStyle::OneHanded;

    unsafe {
        let equip_slot_indices = (equipment as *mut _ as *mut u8).add(0x8) as *mut u32;
        *equip_slot_indices.add(slot) = inventory_index as u32;
    }

    debug_log!(
        "[ignite_overlay] Equipped weapon_id={} to right hand slot 1 from inventory_index={} gaitem=0x{:08x}; wrote equip slot index and menu entry",
        weapon_id,
        inventory_index,
        gaitem_handle.0
    );

    true
}
