// use crate::debug_log;
use eldenring::cs::{WorldChrMan, ItemId, ItemCategory};
use fromsoftware_shared::singleton::get_instance;

#[inline(always)]
pub fn get_key_item_quantity(key_item_id: i32) -> u32 {
    let Some(world_chr_man) = (unsafe { get_instance::<WorldChrMan>() }) else { return 0 };
    let Some(player) = &world_chr_man.main_player else { return 0 };

    let items = &player.player_game_data.equipment.equip_inventory_data.items_data;
    let item_id = ItemId::from_parts(key_item_id, ItemCategory::Goods);

    for entry in items.key_items() {
        if entry.item_id == item_id {
            return entry.quantity;
        }
    }
    0
}
