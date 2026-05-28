use eldenring::cs::WorldChrMan;
use fromsoftware_shared::singleton::get_instance;

use crate::debug_log;

#[derive(Clone, Copy, Debug)]
pub struct RegionStatProfile {
    pub vigor: u32,
    pub mind: u32,
    pub endurance: u32,
    pub strength: u32,
    pub dexterity: u32,
    pub intelligence: u32,
    pub faith: u32,
    pub arcane: u32,
}

pub fn try_apply_region_stats(profile: RegionStatProfile) -> Result<(), String> {
    unsafe {
        let Some(world_chr_man) = get_instance::<WorldChrMan>() else {
            return Err("WorldChrMan unavailable".to_string());
        };

        let Some(player) = &mut world_chr_man.main_player else {
            return Err("main_player unavailable".to_string());
        };

        let attrs = player.player_game_data.as_mut();

        attrs.vigor = profile.vigor;
        attrs.mind = profile.mind;
        attrs.endurance = profile.endurance;
        attrs.strength = profile.strength;
        attrs.dexterity = profile.dexterity;
        attrs.intelligence = profile.intelligence;
        attrs.faith = profile.faith;
        attrs.arcane = profile.arcane;

        debug_log!(
            "[ignite_overlay] Region stats written: VIG={} MND={} END={} STR={} DEX={} INT={} FAI={} ARC={}",
            profile.vigor,
            profile.mind,
            profile.endurance,
            profile.strength,
            profile.dexterity,
            profile.intelligence,
            profile.faith,
            profile.arcane
        );

        Ok(())
    }
}