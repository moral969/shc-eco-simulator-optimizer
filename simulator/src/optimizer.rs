//! Оптимизатор расстановки: параллельный симулированный отжиг + архив лучших решений.
//! Логику ядра не трогает — только вызывает публичные функции Simulator.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use rand::{Rng, seq::IndexedRandom};
use serde::{Deserialize, Serialize};

use crate::{
    BuildingType, Footprint, PopulationEconomySettings, ProjectFile, SimulationSettings,
    Simulator, StockpileResource, walls::line_cells,
};

/// Отступ от края карты, при котором place_building/place_wall не расширяют карту.
const MARGIN: i32 = 55;

#[derive(Debug, Clone)]
pub struct OptimizerConfig {
    /// Итераций на один поток.
    pub max_iterations: usize,
    pub threads: usize,
    /// Сколько лучших решений хранить.
    pub top_n: usize,
    /// Минимум отличающихся зданий, чтобы решения считались разными.
    pub min_diff: usize,
    pub initial_temperature: f64,
    pub final_temperature: f64,
    pub output_dir: PathBuf,
}

impl Default for OptimizerConfig {
    fn default() -> Self {
        let threads = thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1).max(1))
            .unwrap_or(1);
        Self {
            max_iterations: 5000,
            threads,
            top_n: 5,
            min_diff: 2,
            initial_temperature: 30.0,
            final_temperature: 0.5,
            output_dir: PathBuf::from("optimizer_results"),
        }
    }
}

/// Шаг порядка построения.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanStep {
    pub building_type: BuildingType,
    pub x: i32,
    pub y: i32,
    /// None — просто здание. Some("top"/"right"/"bottom"/"left") — сначала временная стена
    /// с этой стороны вплотную, потом здание, потом стена снимается (вход остаётся повёрнутым).
    pub temp_wall_side: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationResult {
    pub gold_per_minute: f64,
    pub worker: usize,
    pub iteration: usize,
    /// Порядок, в котором выжившие здания были поставлены в последний раз.
    pub build_order: Vec<PlanStep>,
    /// Точное решение (со входами). Совместимо с File -> Open.
    pub project: ProjectFile,
}

#[derive(Debug, Clone)]
pub enum OptimizerEvent {
    Log(String),
    Progress {
        elapsed_s: f64,
        evaluated: u64,
        per_sec: f64,
        done: f64,
        best: Option<f64>,
    },
    Finished {
        best: Option<f64>,
        evaluated: u64,
        elapsed_s: f64,
        output_dir: PathBuf,
    },
}

pub struct OptimizerHandle {
    pub cancel: Arc<AtomicBool>,
    pub events: mpsc::Receiver<OptimizerEvent>,
}

pub struct Optimizer;

impl Optimizer {
    pub fn start(
        config: OptimizerConfig,
        settings: SimulationSettings,
        population: PopulationEconomySettings,
        layout: Simulator,
    ) -> OptimizerHandle {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_thread = Arc::clone(&cancel);
        thread::Builder::new()
            .name("optimizer".into())
            .spawn(move || coordinator(config, settings, population, layout, cancel_for_thread, tx))
            .expect("optimizer thread");
        OptimizerHandle { cancel, events: rx }
    }
}

// ---------------------------------------------------------------- shared state

struct Entry {
    result: OptimizationResult,
    keys: HashSet<String>,
}

struct Archive {
    entries: Vec<Entry>,
    top_n: usize,
    min_diff: usize,
    dirty: bool,
}

impl Archive {
    fn threshold(&self) -> f64 {
        if self.entries.len() < self.top_n {
            f64::NEG_INFINITY
        } else {
            self.entries
                .last()
                .map_or(f64::NEG_INFINITY, |e| e.result.gold_per_minute)
        }
    }

    fn best(&self) -> Option<(f64, usize)> {
        self.entries
            .first()
            .map(|e| (e.result.gold_per_minute, e.result.worker))
    }

    fn offer(&mut self, entry: Entry) {
        let score = entry.result.gold_per_minute;
        if score <= self.threshold() {
            return;
        }
        let similar = self
            .entries
            .iter()
            .position(|e| entry.keys.difference(&e.keys).count() < self.min_diff);
        match similar {
            Some(pos) if score > self.entries[pos].result.gold_per_minute => {
                self.entries[pos] = entry;
            }
            Some(_) => return,
            None => self.entries.push(entry),
        }
        self.entries.sort_by(|a, b| {
            b.result
                .gold_per_minute
                .partial_cmp(&a.result.gold_per_minute)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.entries.truncate(self.top_n);
        self.dirty = true;
    }
}

struct Shared {
    cfg: OptimizerConfig,
    settings: SimulationSettings,
    population: PopulationEconomySettings,
    cancel: Arc<AtomicBool>,
    archive: Mutex<Archive>,
    evaluated: AtomicU64,
    iterations: AtomicU64,
}

impl Shared {
    fn offer(&self, worker: usize, iteration: usize, score: f64, sim: &Simulator, plan: &[PlanStep]) {
        {
            let a = self.archive.lock().unwrap_or_else(|e| e.into_inner());
            if score <= a.threshold() {
                return;
            }
        }
        let entry = Entry {
            keys: layout_keys(sim),
            result: OptimizationResult {
                gold_per_minute: score,
                worker,
                iteration,
                build_order: plan.to_vec(),
                project: ProjectFile::capture(sim, self.settings, self.population),
            },
        };
        self.archive
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .offer(entry);
    }
}

fn layout_keys(sim: &Simulator) -> HashSet<String> {
    sim.buildings()
        .iter()
        .filter(|b| !b.anchored)
        .map(|b| {
            format!(
                "{:?}@{},{}>{:?}",
                b.building_type,
                b.x,
                b.y,
                b.entry_point.map(|e| (e.x, e.y))
            )
        })
        .collect()
}

// ---------------------------------------------------------------- evaluation

fn is_workshop(t: BuildingType) -> bool {
    matches!(
        t,
        BuildingType::FletchersWorkshop
            | BuildingType::BlacksmithsWorkshop
            | BuildingType::PoleturnersWorkshop
            | BuildingType::ArmourersWorkshop
    )
}

fn movable(sim: &Simulator) -> Vec<(u32, BuildingType, i32, i32)> {
    sim.buildings()
        .iter()
        .filter(|b| {
            !b.anchored
                && !matches!(
                    b.building_type,
                    BuildingType::Stockpile | BuildingType::GoodsYard
                )
        })
        .map(|b| (b.id, b.building_type, b.x, b.y))
        .collect()
}

/// Чистое золото/мин по всем мастерским (как в GUI: выручка минус закупка ресурсов).
/// Железные шахты из population-economy здесь не учитываются.
fn evaluate(sim: &mut Simulator, settings: SimulationSettings) -> f64 {
    sim.calculate_worker_distances();

    let mut armouries: Vec<u32> = sim
        .buildings()
        .iter()
        .filter(|b| b.building_type == BuildingType::Armoury)
        .map(|b| b.id)
        .collect();
    armouries.sort_unstable();
    if armouries.is_empty() {
        return 0.0;
    }
    let workshops: Vec<(u32, BuildingType)> = sim
        .buildings()
        .iter()
        .filter(|b| is_workshop(b.building_type))
        .map(|b| (b.id, b.building_type))
        .collect();

    let mut total = 0.0;
    for (workshop_id, workshop_type) in workshops {
        let Some(weapon) = settings.selected_weapon_for(workshop_type) else {
            continue;
        };
        let recipe = weapon.recipe();
        for &armoury_id in &armouries {
            if let Ok(cycle) = sim.calculate_production_cycle(weapon, workshop_id, armoury_id, settings)
            {
                if cycle.total_ticks > 0 {
                    let gross = cycle.timing.output_per_cycle * f64::from(recipe.sell_gold);
                    let net = gross - f64::from(settings.resource_buy_gold_per_cycle(recipe));
                    total += net / cycle.total_ticks as f64
                        * f64::from(settings.game_speed_ticks_per_second)
                        * 60.0;
                }
                break; // как в GUI: первая подходящая оружейка
            }
        }
    }
    total
}

fn preflight(sim: &Simulator, settings: SimulationSettings) -> Vec<String> {
    let mut errors = Vec::new();
    let b = sim.buildings();
    if !b.iter().any(|x| x.building_type == BuildingType::Armoury) {
        errors.push("нет Armoury".to_string());
    }
    let workshops: Vec<_> = b.iter().filter(|x| is_workshop(x.building_type)).collect();
    if workshops.is_empty() {
        errors.push("нет ни одной мастерской".to_string());
    }
    if movable(sim).is_empty() {
        errors.push("все здания заякорены — двигать нечего".to_string());
    }
    let mut need = Vec::new();
    for w in &workshops {
        if let Some(weapon) = settings.selected_weapon_for(w.building_type) {
            let res = if weapon.recipe().wood_required > 0 {
                StockpileResource::Wood
            } else {
                StockpileResource::Iron
            };
            if !need.contains(&res) {
                need.push(res);
            }
        }
    }
    for res in need {
        if !b.iter().any(|s| s.stockpile_resource == Some(res)) {
            errors.push(format!("не назначен склад ресурса {}", res.display_name()));
        }
    }
    errors
}

// ---------------------------------------------------------------- mutations

enum Change {
    Removed { x: i32, y: i32 },
    Placed(PlanStep),
}

fn apply_changes(plan: &mut Vec<PlanStep>, changes: Vec<Change>) {
    for change in changes {
        match change {
            Change::Removed { x, y } => plan.retain(|s| (s.x, s.y) != (x, y)),
            Change::Placed(step) => plan.push(step),
        }
    }
}

fn mutate(
    base: &Simulator,
    rng: &mut impl Rng,
    progress: f64,
) -> Option<(Simulator, Vec<Change>)> {
    let mut sim = base.clone();
    sim.set_defer_path_calculation(true);
    let mut changes = Vec::new();
    let roll = rng.random_range(0..100);
    let done = if roll < 45 {
        relocate(&mut sim, rng, progress, &mut changes)
    } else if roll < 80 {
        wall_trick(&mut sim, rng, progress, &mut changes)
    } else {
        swap(&mut sim, rng, &mut changes)
    };
    done.map(|_| (sim, changes))
}

fn pick_position(
    sim: &Simulator,
    rng: &mut impl Rng,
    t: BuildingType,
    around: (i32, i32),
    radius: i32,
) -> Option<(i32, i32)> {
    let bounds = sim.map_bounds();
    let fp = Footprint::for_type(t);
    let (w, h) = (fp.width() as i32, fp.height() as i32);
    let (min_x, max_x) = (bounds.min_x + MARGIN, bounds.max_x - MARGIN - w);
    let (min_y, max_y) = (bounds.min_y + MARGIN, bounds.max_y - MARGIN - h);
    if min_x > max_x || min_y > max_y {
        return None;
    }
    let x = (around.0 + rng.random_range(-radius..=radius)).clamp(min_x, max_x);
    let y = (around.1 + rng.random_range(-radius..=radius)).clamp(min_y, max_y);
    Some((x, y))
}

fn pick_target(
    sim: &Simulator,
    rng: &mut impl Rng,
    id: u32,
    old: (i32, i32),
    progress: f64,
) -> Option<((i32, i32), i32)> {
    let spread = 1.0 - progress;
    if rng.random_bool(0.5) {
        Some((old, 1 + (8.0 * spread) as i32))
    } else {
        let others: Vec<(i32, i32)> = sim
            .buildings()
            .iter()
            .filter(|b| b.id != id)
            .map(|b| (b.x, b.y))
            .collect();
        Some((*others.choose(rng)?, 6 + (24.0 * spread) as i32))
    }
}

fn relocate(
    sim: &mut Simulator,
    rng: &mut impl Rng,
    progress: f64,
    changes: &mut Vec<Change>,
) -> Option<()> {
    let list = movable(sim);
    let &(id, t, ox, oy) = list.choose(rng)?;
    let (around, radius) = pick_target(sim, rng, id, (ox, oy), progress)?;
    if !sim.remove_building(id) {
        return None;
    }
    changes.push(Change::Removed { x: ox, y: oy });
    for _ in 0..30 {
        let (x, y) = pick_position(sim, rng, t, around, radius)?;
        if (x, y) == (ox, oy) {
            continue;
        }
        if sim.place_building(t, x, y).is_ok() {
            changes.push(Change::Placed(PlanStep {
                building_type: t,
                x,
                y,
                temp_wall_side: None,
            }));
            return Some(());
        }
    }
    None
}

/// Стена вплотную к стороне -> мастерская -> стена убирается. Вход остаётся повёрнутым.
/// Стороны соответствуют workshop_wall_rotation_steps: top->0, right->1, bottom->2, left->3.
fn wall_trick(
    sim: &mut Simulator,
    rng: &mut impl Rng,
    progress: f64,
    changes: &mut Vec<Change>,
) -> Option<()> {
    let list: Vec<_> = movable(sim)
        .into_iter()
        .filter(|m| is_workshop(m.1))
        .collect();
    let &(id, t, ox, oy) = list.choose(rng)?;
    let (around, radius) = pick_target(sim, rng, id, (ox, oy), progress)?;
    let same_place = rng.random_bool(0.4);
    if !sim.remove_building(id) {
        return None;
    }
    changes.push(Change::Removed { x: ox, y: oy });

    const N: i32 = 4;
    const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];
    for _ in 0..30 {
        let (x, y) = if same_place {
            (ox, oy)
        } else {
            pick_position(sim, rng, t, around, radius)?
        };
        let rot = rng.random_range(0..4usize);
        let (sx, sy, ex, ey) = match rot {
            0 => (x, y + N, x + N - 1, y + N),
            1 => (x + N, y, x + N, y + N - 1),
            2 => (x, y - 1, x + N - 1, y - 1),
            _ => (x - 1, y, x - 1, y + N - 1),
        };
        let bounds = sim.map_bounds();
        let side_free = line_cells(sx, sy, ex, ey)
            .iter()
            .all(|&(cx, cy)| bounds.contains(cx, cy) && !sim.is_cell_occupied(cx, cy));
        if !side_free {
            continue;
        }
        let mut trial = sim.clone();
        if trial.place_wall(sx, sy, ex, ey).is_err() {
            continue;
        }
        if trial.place_building(t, x, y).is_err() {
            continue;
        }
        // клетки были свободны, поэтому последняя стена в этой клетке — наша
        trial.remove_whole_wall_at(sx, sy);
        *sim = trial;
        changes.push(Change::Placed(PlanStep {
            building_type: t,
            x,
            y,
            temp_wall_side: Some(SIDES[rot].to_string()),
        }));
        return Some(());
    }
    None
}

/// Обмен местами двух разных зданий 4x4 (мастерские / оружейка).
fn swap(sim: &mut Simulator, rng: &mut impl Rng, changes: &mut Vec<Change>) -> Option<()> {
    let list: Vec<_> = movable(sim)
        .into_iter()
        .filter(|m| m.1 == BuildingType::Armoury || is_workshop(m.1))
        .collect();
    let &(ia, ta, xa, ya) = list.choose(rng)?;
    let &(ib, tb, xb, yb) = list.choose(rng)?;
    if ta == tb {
        return None;
    }
    if !(sim.remove_building(ia) && sim.remove_building(ib)) {
        return None;
    }
    changes.push(Change::Removed { x: xa, y: ya });
    changes.push(Change::Removed { x: xb, y: yb });
    sim.place_building(ta, xb, yb).ok()?;
    sim.place_building(tb, xa, ya).ok()?;
    for (t, x, y) in [(ta, xb, yb), (tb, xa, ya)] {
        changes.push(Change::Placed(PlanStep {
            building_type: t,
            x,
            y,
            temp_wall_side: None,
        }));
    }
    Some(())
}

// ---------------------------------------------------------------- worker

fn run_worker(id: usize, start: Simulator, sh: Arc<Shared>) {
    let mut rng = rand::rng();
    let settings = sh.settings;
    let iters = sh.cfg.max_iterations.max(1);
    let t0 = sh.cfg.initial_temperature.max(1e-6);
    let t1 = sh.cfg.final_temperature.max(1e-6).min(t0);

    let mut cur = start;
    cur.set_defer_path_calculation(true);
    let mut plan: Vec<PlanStep> = movable(&cur)
        .into_iter()
        .map(|(_, t, x, y)| PlanStep {
            building_type: t,
            x,
            y,
            temp_wall_side: None,
        })
        .collect();

    // Потоки кроме первого стартуют из «перемешанной» расстановки — для разнообразия.
    if id > 0 {
        for _ in 0..plan.len() * 3 {
            if sh.cancel.load(Ordering::Relaxed) {
                return;
            }
            if let Some((s, ch)) = mutate(&cur, &mut rng, 0.0) {
                cur = s;
                apply_changes(&mut plan, ch);
            }
        }
    }

    let mut cur_score = evaluate(&mut cur, settings);
    sh.evaluated.fetch_add(1, Ordering::Relaxed);
    let mut best_score = cur_score;
    let mut best = (cur.clone(), plan.clone());
    sh.offer(id, 0, best_score, &best.0, &best.1);

    let patience = (iters / 8).max(300);
    let mut last_improve = 0usize;

    for i in 0..iters {
        if sh.cancel.load(Ordering::Relaxed) {
            break;
        }
        let progress = i as f64 / iters as f64;
        let temp = t0 * (t1 / t0).powf(progress);

        if let Some((mut cand, changes)) = mutate(&cur, &mut rng, progress) {
            let score = evaluate(&mut cand, settings);
            sh.evaluated.fetch_add(1, Ordering::Relaxed);
            let delta = score - cur_score;
            if delta >= 0.0 || rng.random::<f64>() < (delta / temp).exp() {
                cur = cand;
                cur_score = score;
                apply_changes(&mut plan, changes);
                if score > best_score + 1e-9 {
                    best_score = score;
                    best = (cur.clone(), plan.clone());
                    last_improve = i;
                    sh.offer(id, i, score, &best.0, &best.1);
                }
            }
        }
        sh.iterations.fetch_add(1, Ordering::Relaxed);

        if i - last_improve > patience {
            cur = best.0.clone();
            plan = best.1.clone();
            cur_score = best_score;
            last_improve = i;
        }
    }
}

// ---------------------------------------------------------------- coordinator

fn coordinator(
    cfg: OptimizerConfig,
    settings: SimulationSettings,
    population: PopulationEconomySettings,
    layout: Simulator,
    cancel: Arc<AtomicBool>,
    tx: mpsc::Sender<OptimizerEvent>,
) {
    let started = Instant::now();
    let dir = cfg.output_dir.clone();
    let send = |event: OptimizerEvent| {
        let _ = tx.send(event);
    };

    let problems = preflight(&layout, settings);
    if !problems.is_empty() {
        for p in problems {
            send(OptimizerEvent::Log(format!("ОШИБКА: {p}")));
        }
        send(OptimizerEvent::Finished {
            best: None,
            evaluated: 0,
            elapsed_s: 0.0,
            output_dir: dir,
        });
        return;
    }

    let threads = cfg.threads.max(1);
    let total_iterations = (threads * cfg.max_iterations.max(1)) as f64;
    send(OptimizerEvent::Log(format!(
        "Старт: {} потоков x {} итераций, подвижных зданий: {}, top {}",
        threads,
        cfg.max_iterations,
        movable(&layout).len(),
        cfg.top_n
    )));

    let shared = Arc::new(Shared {
        archive: Mutex::new(Archive {
            entries: Vec::new(),
            top_n: cfg.top_n.max(1),
            min_diff: cfg.min_diff.max(1),
            dirty: false,
        }),
        cfg,
        settings,
        population,
        cancel,
        evaluated: AtomicU64::new(0),
        iterations: AtomicU64::new(0),
    });

    let mut handles = Vec::new();
    for worker in 0..threads {
        let sh = Arc::clone(&shared);
        let start = layout.clone();
        handles.push(
            thread::Builder::new()
                .name(format!("optimizer-{worker}"))
                .spawn(move || run_worker(worker, start, sh))
                .expect("optimizer worker"),
        );
    }

    let mut last_save = Instant::now();
    let mut last_best = f64::NEG_INFINITY;
    loop {
        thread::sleep(Duration::from_millis(500));
        let finished = handles.iter().all(|h| h.is_finished());

        let best = {
            let mut a = shared.archive.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((score, worker)) = a.best() {
                if score > last_best + 1e-9 {
                    last_best = score;
                    send(OptimizerEvent::Log(format!(
                        "новый рекорд: {score:.2} gold/min (поток {worker})"
                    )));
                }
            }
            if a.dirty && (finished || last_save.elapsed() > Duration::from_secs(2)) {
                if let Err(error) = save_archive(&dir, &a.entries) {
                    send(OptimizerEvent::Log(format!("ошибка сохранения: {error}")));
                }
                a.dirty = false;
                last_save = Instant::now();
            }
            a.best().map(|b| b.0)
        };

        let evaluated = shared.evaluated.load(Ordering::Relaxed);
        let elapsed = started.elapsed().as_secs_f64();
        send(OptimizerEvent::Progress {
            elapsed_s: elapsed,
            evaluated,
            per_sec: evaluated as f64 / elapsed.max(0.001),
            done: (shared.iterations.load(Ordering::Relaxed) as f64 / total_iterations).min(1.0),
            best,
        });

        if finished {
            break;
        }
    }
    for h in handles {
        let _ = h.join();
    }

    let best = shared
        .archive
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .best()
        .map(|b| b.0);
    send(OptimizerEvent::Finished {
        best,
        evaluated: shared.evaluated.load(Ordering::Relaxed),
        elapsed_s: started.elapsed().as_secs_f64(),
        output_dir: dir,
    });
}

fn save_archive(dir: &Path, entries: &[Entry]) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    for (index, entry) in entries.iter().enumerate() {
        let json = serde_json::to_string_pretty(&entry.result.project)?;
        fs::write(dir.join(format!("top_{:02}.json", index + 1)), json)?;
    }
    let report: Vec<&OptimizationResult> = entries.iter().map(|e| &e.result).collect();
    fs::write(dir.join("report.json"), serde_json::to_string_pretty(&report)?)?;
    Ok(())
}