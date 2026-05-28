use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct RegionSchedule {
    pub schedule_name: String,
    pub count_mode: String,
    pub time_basis: String,
    pub phases: Vec<SchedulePhase>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SchedulePhase {
    pub name: String,
    pub region: String,
    pub duration_minutes: u64,
}

pub fn active_phase_index(schedule: &RegionSchedule, igt_seconds: u64) -> Option<usize> {
    let mut start = 0u64;

    for (idx, phase) in schedule.phases.iter().enumerate() {
        let end = start + phase.duration_minutes * 60;
        if igt_seconds >= start && igt_seconds < end {
            return Some(idx);
        }
        start = end;
    }

    None
}