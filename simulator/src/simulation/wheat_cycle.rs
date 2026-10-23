use std::collections::HashMap;

use crate::{BuildingPlacement, CellMap, EntryPoint};

use super::cycle_timing::{CycleTiming, WorkerClock, YieldCounter};
use super::pathfinding::crop_route;

const CROP_COUNT: usize = 36;
const FARM_PULSE: u16 = 151;
const WALK_TICKS_PER_TILE: u64 = 16;
const BUNDLES_PER_FIELD: u32 = 12;
const SAMPLE_FIELDS: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Task {
    Prepare,
    Sow,
    Harvest,
    Collect,
    Wait,
}

#[derive(Clone, Copy)]
enum State {
    Initial,
    Idle,
    Return,
    ApproachPrepare,
    Prepare,
    Sow,
    ApproachHarvest,
    Harvest,
    Collect,
    FindStock,
    Deliver,
}

struct Farmer<'a> {
    map: &'a CellMap,
    home: EntryPoint,
    stock: EntryPoint,
    targets: [EntryPoint; CROP_COUNT],
    crops: [u8; CROP_COUNT],
    task: Task,
    pulse: u16,
    state: State,
    clock: WorkerClock,
    short_idle: bool,
    timer: i32,
    return_setup: bool,
    target: usize,
    position: EntryPoint,
    walk_end: EntryPoint,
    walk_remaining: u64,
    paths: HashMap<(EntryPoint, EntryPoint, bool), (u32, EntryPoint)>,
    harvest_counter: u8,
    carry: u32,
    yield_counter: YieldCounter,
    fear: i32,
}

fn crop_targets(farm: &BuildingPlacement) -> [EntryPoint; CROP_COUNT] {
    let mut offsets = Vec::with_capacity(CROP_COUNT);
    if farm.wheat_layout == 0 {
        for y in [1, 4, 7] {
            for x in 3..=8 {
                offsets.push((x, y));
            }
            for x in (3..=8).rev() {
                offsets.push((x, y + 1));
            }
        }
    } else {
        for x in [4, 7] {
            for y in 0..9 {
                offsets.push((x, y));
                offsets.push((x + 1, y));
            }
        }
    }
    std::array::from_fn(|index| EntryPoint {
        x: farm.x + offsets[index].0,
        y: farm.y + 8 - offsets[index].1,
    })
}

impl Farmer<'_> {
    fn pulse(&mut self) {
        self.pulse += 1;
        if self.pulse < FARM_PULSE {
            return;
        }
        self.pulse = 0;
        for crop in &mut self.crops {
            if (2..103).contains(crop) {
                *crop += 1;
            }
        }
        self.task = choose_task(&self.crops);
    }

    fn select(&self) -> Option<usize> {
        self.crops.iter().position(|crop| match self.task {
            Task::Prepare => *crop == 0 || (101..=120).contains(crop),
            Task::Sow => *crop == 1,
            Task::Harvest => (11..=100).contains(crop),
            Task::Collect => *crop == 121,
            Task::Wait => false,
        })
    }

    fn walk(&mut self, target: EntryPoint, trim: bool) -> Option<()> {
        let key = (self.position, target, trim);
        let (steps, end) = if let Some(path) = self.paths.get(&key) {
            *path
        } else {
            let path = crop_route(self.map, self.position, target, trim)?;
            self.paths.insert(key, path);
            path
        };
        self.walk_end = end;
        self.walk_remaining = u64::from(steps) * WALK_TICKS_PER_TILE;
        Some(())
    }

    fn select_and_walk(&mut self, trim: bool) -> Option<bool> {
        let Some(target) = self.select() else {
            return Some(false);
        };
        self.target = target;
        self.walk(self.targets[target], trim)?;
        self.state = match self.task {
            Task::Prepare => State::ApproachPrepare,
            Task::Sow => State::Sow,
            Task::Harvest => State::ApproachHarvest,
            Task::Collect => State::Collect,
            Task::Wait => return Some(false),
        };
        Some(true)
    }

    fn return_home(&mut self) {
        self.state = State::Return;
        self.timer = 20;
        self.return_setup = true;
    }

    fn update(&mut self) -> Option<u32> {
        self.pulse();
        self.walk_remaining = self.walk_remaining.saturating_sub(1);
        if self.walk_remaining == 0 {
            self.position = self.walk_end;
        }
        self.clock.tick();
        match self.state {
            State::Initial => self.return_home(),
            State::Return => {
                if self.return_setup {
                    self.timer += 1;
                    if self.timer < 20 {
                        return Some(0);
                    }
                    self.timer = 0;
                    self.walk(self.home, false)?;
                    self.return_setup = false;
                }
                self.clock.index = 0;
                if self.walk_remaining == 0 {
                    self.state = State::Idle;
                }
            }
            State::Idle => {
                self.clock.delay = 2;
                let length = if self.short_idle { 52 } else { 144 };
                if self.clock.index >= length {
                    self.clock.index = 0;
                    self.select_and_walk(true)?;
                }
            }
            State::ApproachPrepare | State::ApproachHarvest if self.walk_remaining == 0 => {
                self.clock.index = 0;
                self.state = if matches!(self.state, State::ApproachPrepare) {
                    State::Prepare
                } else {
                    State::Harvest
                };
            }
            State::Prepare => {
                self.short_idle = true;
                if self.clock.index == 8 {
                    self.crops[self.target] = 1;
                }
                if self.clock.index >= 16
                    && (self.task != Task::Prepare || !self.select_and_walk(true)?)
                {
                    self.return_home();
                    self.clock.index = 0;
                }
            }
            State::Sow => {
                self.short_idle = false;
                self.clock.index = 0;
                if self.walk_remaining == 0 {
                    self.crops[self.target] = 2;
                    if self.task != Task::Sow || !self.select_and_walk(false)? {
                        self.return_home();
                    }
                }
            }
            State::Harvest => {
                if self.clock.index == 1 && self.crops[self.target] < 120 {
                    self.harvest_counter += 1;
                    self.crops[self.target] = if self.harvest_counter == 3 {
                        self.harvest_counter = 0;
                        121
                    } else {
                        120
                    };
                }
                if self.clock.index >= 16
                    && (!matches!(self.task, Task::Harvest | Task::Collect)
                        || !self.select_and_walk(true)?)
                {
                    self.return_home();
                }
            }
            State::Collect if self.walk_remaining == 0 => {
                self.crops[self.target] = 120;
                self.state = State::FindStock;
                self.clock.index = 0;
            }
            State::FindStock => {
                self.timer += 1;
                if self.timer >= 11 {
                    self.timer = 0;
                    self.walk(self.stock, false)?;
                    self.carry = self.yield_counter.take(2, self.fear, false);
                    self.state = State::Deliver;
                }
            }
            State::Deliver => {
                self.timer += 1;
                if self.timer >= 5 && self.walk_remaining == 0 {
                    self.timer = 0;
                    self.carry -= 1;
                    if self.carry == 0
                        && (!matches!(self.task, Task::Harvest | Task::Collect)
                            || !self.select_and_walk(true)?)
                    {
                        self.return_home();
                    }
                    return Some(1);
                }
            }
            _ => {}
        }
        Some(0)
    }
}

fn choose_task(crops: &[u8; CROP_COUNT]) -> Task {
    let count = |predicate: fn(u8) -> bool| crops.iter().filter(|&&crop| predicate(crop)).count();
    let bundles = count(|crop| crop == 121);
    let ripe = count(|crop| (11..=100).contains(&crop));
    if bundles >= 4 {
        Task::Collect
    } else if ripe > 11 || (ripe > 0 && crops.contains(&120)) {
        Task::Harvest
    } else if bundles > 0 {
        Task::Collect
    } else if crops
        .iter()
        .any(|crop| *crop == 0 || (102..=120).contains(crop))
    {
        Task::Prepare
    } else if crops.contains(&1) {
        Task::Sow
    } else {
        Task::Wait
    }
}

pub(super) fn wheat_cycle(
    map: &CellMap,
    farm: &BuildingPlacement,
    stock: EntryPoint,
    fear: i32,
) -> Option<CycleTiming> {
    let home = farm.entry_point?;
    let mut farmer = Farmer {
        map,
        home,
        stock,
        targets: crop_targets(farm),
        crops: [0; CROP_COUNT],
        task: Task::Prepare,
        pulse: 0,
        state: State::Initial,
        clock: WorkerClock::new(2),
        short_idle: false,
        timer: 0,
        return_setup: false,
        target: 0,
        position: home,
        walk_end: home,
        walk_remaining: 0,
        paths: HashMap::new(),
        harvest_counter: 0,
        carry: 0,
        yield_counter: YieldCounter::default(),
        fear,
    };
    let mut batches = Vec::with_capacity(SAMPLE_FIELDS);
    let mut bundles = 0;
    let mut batch_output = 0;
    for tick in 1..=2_000_000 {
        let delivered = farmer.update()?;
        batch_output += delivered;
        if delivered > 0 && farmer.carry == 0 {
            bundles += 1;
            if bundles == BUNDLES_PER_FIELD {
                batches.push((tick, batch_output));
                bundles = 0;
                batch_output = 0;
                if batches.len() == SAMPLE_FIELDS {
                    return Some(CycleTiming::from_batches(&batches));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuildingType, Footprint};

    fn fixture(layout: u8) -> (CellMap, BuildingPlacement) {
        let farm = BuildingPlacement {
            id: 1,
            building_type: BuildingType::WheatFarm,
            x: 100,
            y: 92,
            wheat_layout: layout,
            goods_yard_group_id: None,
            stockpile_resource: None,
            entry_point: Some(EntryPoint { x: 101, y: 97 }),
            footprint: Footprint::for_type(BuildingType::WheatFarm),
            components: vec![],
        };
        let mut map = CellMap::new(220);
        map.place(&farm).expect("farm fixture");
        (map, farm)
    }

    #[test]
    fn reproduces_original_instruction_wheat_trace_with_same_flat_walking() {
        let (map, farm) = fixture(0);
        let cycle =
            wheat_cycle(&map, &farm, EntryPoint { x: 101, y: 93 }, 0).expect("reachable field");
        assert_eq!(cycle.first_delivery_ticks, 8184);
        assert_eq!(cycle.mean_ticks, 8154.0);
        assert_eq!(cycle.output_per_cycle, 24.0);
    }

    #[test]
    fn farm_layout_and_stock_side_are_actual_inputs() {
        let (map, farm) = fixture(0);
        let south = wheat_cycle(&map, &farm, EntryPoint { x: 101, y: 93 }, 0).expect("south");
        let west = wheat_cycle(&map, &farm, EntryPoint { x: 97, y: 97 }, 0).expect("west");
        assert_ne!(south.mean_ticks, west.mean_ticks);
        let (_, other) = fixture(1);
        assert_ne!(crop_targets(&farm), crop_targets(&other));
    }

    #[test]
    fn crop_priority_preserves_ripe_threshold_and_bundle_priority() {
        let mut crops = [2; 36];
        crops[..11].fill(11);
        assert_eq!(choose_task(&crops), Task::Wait);
        crops[11] = 11;
        assert_eq!(choose_task(&crops), Task::Harvest);
        crops[32..].fill(121);
        assert_eq!(choose_task(&crops), Task::Collect);
    }
}
