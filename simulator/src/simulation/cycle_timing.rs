//! Successful full production cycles on flat, uncrowded routes.
//! Native clocks/gates are retained; input availability is handled by the economy report.

use super::{SimulationSettings, WeaponType};

const SAMPLE_BATCHES: usize = 22;
const SKIP_BATCHES: usize = 2;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CycleTiming {
    pub first_delivery_ticks: u64,
    pub mean_ticks: f64,
    pub min_ticks: u64,
    pub max_ticks: u64,
    pub output_per_cycle: f64,
}

impl CycleTiming {
    pub fn output_per_tick(self) -> f64 {
        if self.mean_ticks > 0.0 {
            self.output_per_cycle / self.mean_ticks
        } else {
            0.0
        }
    }

    pub(super) fn from_batches(batches: &[(u64, u32)]) -> Self {
        let intervals: Vec<_> = batches
            .windows(2)
            .skip(SKIP_BATCHES - 1)
            .map(|pair| (pair[1].0 - pair[0].0, pair[1].1))
            .collect();
        if intervals.is_empty() {
            return Self::default();
        }
        Self {
            first_delivery_ticks: batches[0].0,
            mean_ticks: intervals.iter().map(|v| v.0 as f64).sum::<f64>() / intervals.len() as f64,
            min_ticks: intervals.iter().map(|v| v.0).min().unwrap_or(0),
            max_ticks: intervals.iter().map(|v| v.0).max().unwrap_or(0),
            output_per_cycle: intervals.iter().map(|v| f64::from(v.1)).sum::<f64>()
                / intervals.len() as f64,
        }
    }
}

#[derive(Default)]
pub(super) struct YieldCounter {
    remainder: u32,
}

impl YieldCounter {
    pub(super) fn take(&mut self, base: u32, fear: i32, mode_bonus: bool) -> u32 {
        let percent = 100
            + super::clamped_fear_factor(fear).unsigned_abs() * 10
            + if mode_bonus { 50 } else { 0 };
        self.remainder += (percent - 100) * base;
        let amount = base + self.remainder / 100;
        self.remainder %= 100;
        amount
    }
}

#[derive(Default, Clone, Copy)]
pub(super) struct WorkerClock {
    pub index: u32,
    pub delay: u64,
    now: u64,
    last: u64,
}

impl WorkerClock {
    pub(super) fn new(delay: u64) -> Self {
        Self {
            delay,
            ..Self::default()
        }
    }
    pub fn tick(&mut self) {
        self.now += 1;
        if self.now - self.last > self.delay {
            self.last = self.now;
            self.index += 1;
            if self.index >= 1000 {
                self.index = 0;
            }
        }
    }
}

struct Animation {
    phases: &'static [u32],
    interval: u64,
    now: u64,
    last: u64,
    index: u32,
    phase: usize,
    mode: u8,
    increment: u32,
    ready: bool,
}

impl Animation {
    fn new(phases: &'static [u32], interval: u64) -> Self {
        Self {
            phases,
            interval,
            now: 0,
            last: 0,
            index: 0,
            phase: 0,
            mode: 0,
            increment: 0,
            ready: false,
        }
    }

    fn tick(&mut self) {
        // An active building restores its run flag after the manager clock.
        // Stop therefore suppresses one manager-clock visit, retaining its phase.
        if self.mode == 0 {
            self.mode = 1;
            return;
        }
        self.now += 1;
        if self.now - self.last >= self.interval {
            self.last = self.now;
            self.index += self.increment;
        }
        if self.index >= self.phases[self.phase] {
            self.index = 0;
            self.phase += 1;
            if self.phase == self.phases.len() {
                self.ready = true;
                self.phase = 0;
            }
        }
    }

    fn freeze(&mut self) {
        self.mode = 3;
        self.increment = 0;
    }
    fn start(&mut self) {
        self.mode = 2;
        self.phase = 0;
        self.index = 0;
        self.ready = false;
    }
    fn poll(&mut self) -> bool {
        self.mode = 1;
        self.increment = 1;
        self.ready
    }
    fn stop(&mut self) {
        self.mode = 0;
        self.index = 0;
        self.ready = false;
    }

    fn advance(&mut self, elapsed: &mut u64, ticks: u64) {
        for _ in 0..ticks {
            *elapsed += 1;
            self.tick();
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct WorkshopPaths {
    pub home_to_input: u64,
    pub input_to_home: u64,
    pub home_to_output: u64,
    pub output_to_input: u64,
    pub output_to_home: u64,
}

#[derive(Clone, Copy)]
enum WorkshopState {
    SelectInput,
    InputIdle,
    Fetch,
    ReturnInput,
    Enter,
    Prepare,
    Process,
    FinishBuffer,
    Leave,
    SelectOutput,
    Deliver,
    ReturnHome,
}

pub(super) fn workshop_cycle(
    weapon: WeaponType,
    paths: WorkshopPaths,
    settings: SimulationSettings,
) -> CycleTiming {
    let (phases, interval): (&'static [u32], u64) = match weapon {
        WeaponType::Bow => (&[145, 0], 4),
        WeaponType::Crossbow => (&[127, 0], 4),
        WeaponType::Spear => (&[104, 19], 2),
        WeaponType::Pike => (&[80, 33, 33, 35], 3),
        WeaponType::Sword => (&[84, 61, 61, 55], 4),
        WeaponType::Mace => (&[30, 74, 110], 4),
        WeaponType::Armor => (&[111, 17], 4),
    };
    let fletcher = matches!(weapon, WeaponType::Bow | WeaponType::Crossbow);
    let transfers = matches!(
        weapon,
        WeaponType::Spear | WeaponType::Pike | WeaponType::Armor
    );
    let direct_return = !fletcher || settings.optimized_fletcher_routing;
    let required = weapon.recipe().total_required_units();
    let mut animation = Animation::new(phases, interval);
    let mut state = WorkshopState::SelectInput;
    let mut remaining = 0_u64;
    let mut timer = 0;
    let mut local_input = 0;
    let mut carry = 0;
    let mut batch_output = 0;
    let mut return_order_pending = false;
    let mut yield_counter = YieldCounter::default();
    let mut worker_clock = WorkerClock::new(2);
    let mut input_idle_sequence = 0;
    let mut batches = Vec::with_capacity(SAMPLE_BATCHES);
    let mut tick = 0;
    while batches.len() < SAMPLE_BATCHES {
        tick += 1;
        animation.tick();
        worker_clock.tick();
        remaining = remaining.saturating_sub(1_u64);
        match state {
            WorkshopState::SelectInput => {
                remaining = paths.home_to_input;
                state = WorkshopState::Fetch;
            }
            WorkshopState::Fetch if remaining == 0 => {
                worker_clock.index = 0;
                return_order_pending = true;
                state = WorkshopState::ReturnInput;
            }
            WorkshopState::ReturnInput if remaining == 0 => {
                if return_order_pending {
                    return_order_pending = false;
                    remaining = paths.input_to_home;
                    if remaining > 0 {
                        continue;
                    }
                }
                local_input += 1;
                if local_input < required {
                    if fletcher {
                        state = WorkshopState::SelectInput;
                    } else if weapon == WeaponType::Pike {
                        state = WorkshopState::InputIdle;
                    } else {
                        remaining = paths.home_to_input;
                        state = WorkshopState::Fetch;
                    }
                } else {
                    worker_clock.index = 0;
                    timer = 0;
                    state = if transfers {
                        WorkshopState::Enter
                    } else {
                        WorkshopState::Prepare
                    };
                }
            }
            WorkshopState::InputIdle => {
                let length = [156, 54, 26, 100][input_idle_sequence];
                if worker_clock.index >= length {
                    worker_clock.index = 0;
                    input_idle_sequence = (input_idle_sequence + 1) % 4;
                    remaining = paths.home_to_input;
                    state = WorkshopState::Fetch;
                }
            }
            WorkshopState::Enter => {
                timer += 1;
                if timer == 17 {
                    timer = 0;
                    animation.start();
                    state = WorkshopState::Prepare;
                }
            }
            WorkshopState::Prepare => {
                timer += 1;
                if timer == 30 {
                    local_input = 0;
                    animation.start();
                    timer = 0;
                    state = WorkshopState::Process;
                } else {
                    animation.freeze();
                }
            }
            WorkshopState::Process if animation.poll() => {
                if weapon == WeaponType::Armor {
                    timer = 0;
                    state = WorkshopState::FinishBuffer;
                    continue;
                }
                animation.stop();
                carry = yield_counter.take(1, settings.fear_factor, true);
                batch_output = carry;
                timer = 0;
                state = if transfers {
                    WorkshopState::Leave
                } else {
                    WorkshopState::SelectOutput
                };
            }
            WorkshopState::FinishBuffer => {
                timer += 1;
                if timer == 30 {
                    animation.stop();
                    carry = yield_counter.take(1, settings.fear_factor, true);
                    batch_output = carry;
                    timer = 0;
                    state = WorkshopState::Leave;
                } else {
                    animation.freeze();
                }
            }
            WorkshopState::Leave => {
                timer += 1;
                if timer == 17 {
                    timer = 0;
                    animation.start();
                    state = WorkshopState::SelectOutput;
                }
            }
            WorkshopState::SelectOutput => {
                timer += 1;
                if timer == 11 {
                    timer = 0;
                    remaining = paths.home_to_output;
                    state = WorkshopState::Deliver;
                }
            }
            WorkshopState::Deliver => {
                timer += 1;
                if timer >= 5 && remaining == 0 {
                    timer = 0;
                    carry -= 1;
                    if carry == 0 {
                        batches.push((tick, batch_output));
                        if direct_return {
                            remaining = paths.output_to_input;
                            state = WorkshopState::Fetch;
                        } else {
                            return_order_pending = true;
                            state = WorkshopState::ReturnHome;
                        }
                    }
                }
            }
            WorkshopState::ReturnHome if remaining == 0 => {
                if return_order_pending {
                    return_order_pending = false;
                    remaining = paths.output_to_home;
                    if remaining > 0 {
                        continue;
                    }
                }
                state = WorkshopState::SelectInput;
            }
            _ => {}
        }
    }
    CycleTiming::from_batches(&batches)
}

pub(super) fn bakery_cycle(
    input_to_home: u64,
    output_to_input: u64,
    home_to_output: u64,
    fear: i32,
) -> CycleTiming {
    let mut animation = Animation::new(&[392, 16], 4);
    let mut yield_counter = YieldCounter::default();
    let mut batches = Vec::with_capacity(SAMPLE_BATCHES);
    let mut tick = 0;
    // Successful repeated native route: granary -> flour -> bakery -> granary.
    for _ in 0..SAMPLE_BATCHES {
        animation.advance(&mut tick, output_to_input.max(1));
        // Input and output yield calls share the worker's fractional remainder.
        yield_counter.take(1, fear, false);
        animation.advance(&mut tick, input_to_home + 1);
        animation.advance(&mut tick, 17);
        animation.start();
        tick += 1;
        animation.tick();
        animation.start();
        loop {
            tick += 1;
            animation.tick();
            if animation.poll() {
                animation.stop();
                break;
            }
        }
        let output = yield_counter.take(8, fear, false);
        animation.advance(&mut tick, 17);
        animation.start();
        animation.advance(&mut tick, 11);
        // The -10 departure timer advances during walking, including the first gate.
        animation.advance(
            &mut tick,
            home_to_output.max(15) + 5 * u64::from(output.saturating_sub(1)),
        );
        batches.push((tick, output));
    }
    CycleTiming::from_batches(&batches)
}

#[derive(Clone, Copy)]
enum MillerState {
    Fetch,
    Return,
    Queue,
    Process,
    Depart,
    Deliver,
}

#[derive(Clone, Copy)]
struct Miller {
    state: MillerState,
    remaining: u64,
    clock: WorkerClock,
}

pub(super) fn mill_cycle(
    flour_to_wheat: u64,
    wheat_to_mill: u64,
    mill_to_flour: u64,
) -> CycleTiming {
    let mut workers = [Miller {
        state: MillerState::Fetch,
        remaining: flour_to_wheat,
        clock: WorkerClock::new(2),
    }; 3];
    let mut active = None;
    let mut process_remaining = 0_u64;
    let mut coast_frames = 0_u8;
    let mut batches = Vec::with_capacity(48);
    let mut tick = 0;
    while batches.len() < 48 {
        tick += 1;
        // With all workers alive, the 15-position wheel/coast sequence must
        // finish before phase zero allows the next processor claim (RVA 2E1BE0).
        if tick % 2 == 0 {
            coast_frames = coast_frames.saturating_sub(1);
        }
        process_remaining = process_remaining.saturating_sub(1);
        // Stable worker slot order reproduces simultaneous processor claims.
        for (id, worker) in workers.iter_mut().enumerate() {
            worker.clock.tick();
            if !matches!(worker.state, MillerState::Depart) {
                worker.clock.index = 0;
            }
            worker.remaining = worker.remaining.saturating_sub(1);
            match worker.state {
                MillerState::Fetch if worker.remaining == 0 => {
                    worker.state = MillerState::Return;
                    worker.remaining = wheat_to_mill + 1;
                }
                MillerState::Return if worker.remaining == 0 => {
                    if active.is_none() && process_remaining == 0 && coast_frames == 0 {
                        active = Some(id);
                        process_remaining = 272 - tick % 2;
                        worker.state = MillerState::Process;
                    } else {
                        worker.state = MillerState::Queue;
                    }
                }
                MillerState::Queue
                    if active.is_none() && process_remaining == 0 && coast_frames == 0 =>
                {
                    active = Some(id);
                    process_remaining = 272 - tick % 2;
                    worker.state = MillerState::Process;
                }
                MillerState::Process if active == Some(id) && process_remaining == 0 => {
                    active = None;
                    coast_frames = 15;
                    worker.state = MillerState::Depart;
                }
                MillerState::Depart => {
                    worker.clock.delay = 0;
                    if worker.clock.index >= 20 {
                        worker.clock.index = 0;
                        worker.state = MillerState::Deliver;
                        worker.remaining = mill_to_flour;
                    }
                }
                MillerState::Deliver if worker.remaining == 0 => {
                    batches.push((tick, 1));
                    worker.state = MillerState::Fetch;
                    worker.remaining = flour_to_wheat;
                }
                _ => {}
            }
        }
    }
    CycleTiming::from_batches(&batches)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fletcher_matches_original_zero_walk_elapsed_fixture() {
        let result = workshop_cycle(
            WeaponType::Crossbow,
            WorkshopPaths {
                home_to_input: 0,
                input_to_home: 0,
                home_to_output: 0,
                output_to_input: 0,
                output_to_home: 0,
            },
            SimulationSettings::default(),
        );
        assert_eq!(result.first_delivery_ticks, 562);
        assert!((result.output_per_cycle - 1.5).abs() < 1e-12);
        assert!((result.mean_ticks - 567.0).abs() < 2.0, "{result:?}");
    }

    #[test]
    fn full_workshop_cycles_match_independent_native_instruction_probes() {
        let paths = WorkshopPaths {
            home_to_input: 0,
            input_to_home: 0,
            home_to_output: 0,
            output_to_input: 0,
            output_to_home: 0,
        };
        for (weapon, expected) in [
            (WeaponType::Spear, 331.0),
            (WeaponType::Sword, 1095.0),
            (WeaponType::Mace, 907.0),
            (WeaponType::Armor, 627.0),
        ] {
            let result = workshop_cycle(weapon, paths, SimulationSettings::default());
            assert!(
                (result.mean_ticks - expected).abs() < 1.0,
                "{weapon:?}: {result:?}"
            );
        }
        let pike = workshop_cycle(WeaponType::Pike, paths, SimulationSettings::default());
        assert!(pike.max_ticks - pike.min_ticks > 350, "{pike:?}");
        let bread = bakery_cycle(0, 0, 0, 0);
        assert!((bread.mean_ticks - 1729.0).abs() < 1.0, "{bread:?}");
    }

    #[test]
    fn moving_cycles_match_native_orders_and_return_setup_delay() {
        let paths = WorkshopPaths {
            home_to_input: 48,
            input_to_home: 48,
            home_to_output: 72,
            output_to_input: 120,
            output_to_home: 72,
        };
        for (weapon, expected) in [
            (WeaponType::Crossbow, 991.0),
            (WeaponType::Spear, 565.0),
            (WeaponType::Sword, 1327.0),
            (WeaponType::Mace, 1139.0),
            (WeaponType::Armor, 859.0),
        ] {
            let cycle = workshop_cycle(weapon, paths, SimulationSettings::default());
            assert!(
                (cycle.mean_ticks - expected).abs() < 1.0,
                "{weapon:?}: {cycle:?}"
            );
        }
        let bread = bakery_cycle(48, 120, 72, 0);
        assert!((bread.mean_ticks - 1953.0).abs() < 1.0, "{bread:?}");
    }

    #[test]
    fn yield_keeps_fractional_carry_for_each_worker() {
        let mut yield_counter = YieldCounter::default();
        assert_eq!(
            (0..4)
                .map(|_| yield_counter.take(1, 0, true))
                .collect::<Vec<_>>(),
            [1, 2, 1, 2]
        );
        assert_eq!(
            (0..5)
                .map(|_| yield_counter.take(2, -2, false))
                .sum::<u32>(),
            12
        );
    }

    #[test]
    fn mill_workers_share_processing_and_transport_can_limit_them() {
        let near = mill_cycle(0, 0, 0);
        let far = mill_cycle(2400, 3200, 3200);
        assert!(
            near.mean_ticks == 302.0 && near.first_delivery_ticks == 296,
            "{near:?}"
        );
        assert!(far.output_per_tick() < near.output_per_tick() / 5.0);
    }

    #[test]
    fn bakery_walk_overlaps_its_first_deposit_gate() {
        let at_door = bakery_cycle(0, 0, 0, 0);
        let walking = bakery_cycle(0, 0, 24, 0);
        assert_eq!(at_door.output_per_cycle, 8.0);
        assert!(walking.mean_ticks - at_door.mean_ticks < 24.0);
        assert!(at_door.mean_ticks > 1632.0);
    }
}
