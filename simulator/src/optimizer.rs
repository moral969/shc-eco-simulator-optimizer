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

use rand::{
    Rng,
    seq::{IndexedRandom, SliceRandom},
};
use serde::{Deserialize, Serialize};

use crate::{
    BuildStep, BuildingType, Footprint, PopulationEconomySettings, ProjectFile, SimulationSettings,
    Simulator, StockpileResource, StyleMetrics, StyleWeights, style_metrics, walls::line_cells,
};

mod genetic;

/// Отступ от края карты, при котором place_building/place_wall не расширяют карту.
const MARGIN: i32 = 55;
/// Сторона мастерской.
const N: i32 = 4;
/// Максимум мастерских в одном ряду под одной стеной.
const MAX_ROW: usize = 8;
const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

/// Алгоритм поиска.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptimizerAlgorithm {
    /// Параллельный симулированный отжиг (поток = независимая цепочка).
    Annealing,
    /// Генетический алгоритм (поток = остров со своей популяцией).
    Genetic,
}

#[derive(Debug, Clone)]
pub struct OptimizerConfig {
    /// Итераций на один поток (для генетики — оценок особей).
    pub max_iterations: usize,
    pub threads: usize,
    /// Сколько лучших решений хранить.
    pub top_n: usize,
    /// Минимум отличающихся зданий, чтобы решения считались разными.
    pub min_diff: usize,
    /// Максимум попыток стены в одном решении (одна попытка = одна стена любой длины,
    /// которая разворачивает сразу ряд мастерских; снимать её необязательно). 0 — стены не используются.
    pub max_walls: usize,
    /// Штраф (gold/мин) за каждую использованную попытку стены: чем больше мастерских
    /// разворачивается одной стеной, тем меньше штрафов и тем удобнее строить.
    pub wall_penalty: f64,
    pub algorithm: OptimizerAlgorithm,
    /// Веса «аккуратности» расстановки (ряды, выравнивание, диагонали, симметрия), gold/мин.
    pub style: StyleWeights,
    /// Генетика: особей в популяции острова.
    pub population: usize,
    /// Генетика: сколько лучших особей переходит в следующее поколение без изменений.
    pub elite: usize,
    /// Генетика: вероятность скрещивания (иначе ребёнок — копия родителя).
    pub crossover_rate: f64,
    /// Генетика: вероятность мутации ребёнка.
    pub mutation_rate: f64,
    /// Генетика: размер турнира при выборе родителей.
    pub tournament: usize,
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
            max_walls: 3,
            wall_penalty: 3.0,
            algorithm: OptimizerAlgorithm::Annealing,
            style: StyleWeights::default(),
            population: 24,
            elite: 2,
            crossover_rate: 0.7,
            mutation_rate: 0.8,
            tournament: 3,
            initial_temperature: 30.0,
            final_temperature: 0.5,
            output_dir: PathBuf::from("optimizer_results"),
        }
    }
}

/// Стена, которой разворачивается целый ряд мастерских (общая для всех мастерских группы).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WallPlan {
    /// Номер группы: мастерские с одним номером развёрнуты одной стеной.
    pub group: u32,
    /// "top"/"right"/"bottom"/"left" — с какой стороны вплотную стоит стена.
    pub side: String,
    pub sx: i32,
    pub sy: i32,
    pub ex: i32,
    pub ey: i32,
    /// true — стену после постройки не снимают.
    pub kept: bool,
}

/// Шаг порядка построения.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanStep {
    pub building_type: BuildingType,
    pub x: i32,
    pub y: i32,
    /// None — просто здание. Some — сначала стена (одна на всю группу), потом мастерские группы,
    /// потом стена снимается, если не kept (вход остаётся повёрнутым).
    pub wall: Option<WallPlan>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationResult {
    /// Чистое золото/мин.
    pub gold_per_minute: f64,
    /// Оценка отжига: золото минус штраф за попытки стен (по ней сортируется архив).
    pub score: f64,
    /// Сколько попыток стены использовано в решении.
    pub walls_used: usize,
    /// Бонус аккуратности, уже входящий в score.
    pub style_bonus: f64,
    /// Метрики аккуратности (0..1).
    pub style: StyleMetrics,
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
                .map_or(f64::NEG_INFINITY, |e| e.result.score)
        }
    }

    /// Чистое золото лучшего решения (по score) и поток, который его нашёл.
    fn best(&self) -> Option<(f64, usize)> {
        self.entries
            .first()
            .map(|e| (e.result.gold_per_minute, e.result.worker))
    }

    fn offer(&mut self, entry: Entry) {
        let score = entry.result.score;
        if score <= self.threshold() {
            return;
        }
        let similar = self
            .entries
            .iter()
            .position(|e| entry.keys.difference(&e.keys).count() < self.min_diff);
        match similar {
            Some(pos) if score > self.entries[pos].result.score => {
                self.entries[pos] = entry;
            }
            Some(_) => return,
            None => self.entries.push(entry),
        }
        self.entries.sort_by(|a, b| {
            b.result
                .score
                .partial_cmp(&a.result.score)
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
    /// Неподвижная часть раскладки: с неё начинается таймлайн.
    fixed_steps: Vec<BuildStep>,
}

impl Shared {
    fn offer(&self, worker: usize, iteration: usize, eval: Eval, sim: &Simulator, plan: &[PlanStep]) {
        {
            let a = self.archive.lock().unwrap_or_else(|e| e.into_inner());
            if eval.score <= a.threshold() {
                return;
            }
        }
        let build_order = normalize_plan(plan);
        let mut project = ProjectFile::capture(sim, self.settings, self.population);
        project.build_sequence = self
            .fixed_steps
            .iter()
            .cloned()
            .chain(plan_steps(&build_order))
            .collect();
        let entry = Entry {
            keys: layout_keys(sim),
            result: OptimizationResult {
                gold_per_minute: eval.gold,
                score: eval.score,
                walls_used: eval.walls_used,
                style_bonus: eval.bonus,
                style: eval.style,
                worker,
                iteration,
                build_order,
                project,
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

/// Для снимаемых стен укорачивает линию до реально оставшихся мастерских группы
/// (часть мастерских могла быть переставлена позже). Оставленные стены не трогает.
fn normalize_plan(plan: &[PlanStep]) -> Vec<PlanStep> {
    let mut out = plan.to_vec();
    let groups: HashSet<u32> = removed_groups(plan.iter());
    for group in groups {
        let idx: Vec<usize> = out
            .iter()
            .enumerate()
            .filter(|(_, s)| s.wall.as_ref().is_some_and(|w| w.group == group))
            .map(|(i, _)| i)
            .collect();
        let Some(&first) = idx.first() else { continue };
        let Some(wall) = out[first].wall.clone() else { continue };
        let horizontal = wall.side == "top" || wall.side == "bottom";
        let (lo, hi) = idx.iter().fold((i32::MAX, i32::MIN), |(lo, hi), &i| {
            let v = if horizontal { out[i].x } else { out[i].y };
            (lo.min(v), hi.max(v))
        });
        for &i in &idx {
            if let Some(w) = out[i].wall.as_mut() {
                if horizontal {
                    w.sx = lo;
                    w.ex = hi + N - 1;
                } else {
                    w.sy = lo;
                    w.ey = hi + N - 1;
                }
            }
        }
    }
    out
}

/// Неподвижная часть исходной раскладки в порядке построения:
/// стены, склады/заякоренные здания, назначения складов.
fn fixed_steps(layout: &Simulator) -> Vec<BuildStep> {
    let mut steps: Vec<BuildStep> = layout
        .walls()
        .iter()
        .map(|w| BuildStep::Wall {
            start_x: w.start_x,
            start_y: w.start_y,
            end_x: w.end_x,
            end_y: w.end_y,
        })
        .collect();
    let free: HashSet<u32> = movable(layout).iter().map(|m| m.0).collect();
    // Goods Yard состоит из 4 стеков: ставим один раз, по левому нижнему углу группы.
    let mut yards: Vec<(u32, i32, i32)> = Vec::new();
    for b in layout.buildings().iter().filter(|b| !free.contains(&b.id)) {
        match b.goods_yard_group_id {
            Some(group) => match yards.iter_mut().find(|y| y.0 == group) {
                Some(y) => {
                    y.1 = y.1.min(b.x);
                    y.2 = y.2.min(b.y);
                }
                None => yards.push((group, b.x, b.y)),
            },
            None => steps.push(BuildStep::Building {
                building_type: b.building_type,
                x: b.x,
                y: b.y,
            }),
        }
    }
    steps.extend(yards.into_iter().map(|(_, x, y)| BuildStep::Building {
        building_type: BuildingType::GoodsYard,
        x,
        y,
    }));
    for b in layout.buildings() {
        if let Some(resource) = b.stockpile_resource {
            steps.push(BuildStep::Stockpile {
                x: b.x,
                y: b.y,
                resource,
            });
        }
    }
    steps
}

/// План оптимизатора -> шаги: у ряда под одной стеной сначала стена, потом все мастерские ряда,
/// потом снятие стены (если не kept).
fn plan_steps(plan: &[PlanStep]) -> Vec<BuildStep> {
    let building = |s: &PlanStep| BuildStep::Building {
        building_type: s.building_type,
        x: s.x,
        y: s.y,
    };
    let mut out = Vec::new();
    let mut handled = HashSet::new();
    for (i, step) in plan.iter().enumerate() {
        let Some(wall) = &step.wall else {
            out.push(building(step));
            continue;
        };
        if !handled.insert(wall.group) {
            continue;
        }
        out.push(BuildStep::Wall {
            start_x: wall.sx,
            start_y: wall.sy,
            end_x: wall.ex,
            end_y: wall.ey,
        });
        out.extend(
            plan[i..]
                .iter()
                .filter(|s| s.wall.as_ref().is_some_and(|w| w.group == wall.group))
                .map(building),
        );
        if !wall.kept {
            out.push(BuildStep::Remove {
                x: wall.sx,
                y: wall.sy,
                whole_wall: true,
            });
        }
    }
    out
}

// ---------------------------------------------------------------- walls budget

#[derive(Debug, Clone, Copy)]
struct WallBudget {
    /// Максимум попыток стены.
    max: usize,
    /// Сколько стен было в исходной раскладке (их не считаем).
    base: usize,
}

/// Номера групп со снимаемой стеной (оставленные стены считаются по самому симулятору).
fn removed_groups<'a>(steps: impl Iterator<Item = &'a PlanStep>) -> HashSet<u32> {
    steps
        .filter_map(|s| s.wall.as_ref().filter(|w| !w.kept).map(|w| w.group))
        .collect()
}

/// Сколько попыток стены использовано: оставленные стены (лишние в симуляторе)
/// плюс группы со снятой стеной, у которых ещё есть мастерские в плане.
fn wall_attempts(sim: &Simulator, plan: &[PlanStep], base: usize) -> usize {
    sim.walls().len().saturating_sub(base) + removed_groups(plan.iter()).len()
}

/// Итог оценки раскладки (общий для отжига и генетики).
#[derive(Debug, Clone, Copy)]
struct Eval {
    gold: f64,
    /// gold − штраф за стены + бонус за аккуратность.
    score: f64,
    walls_used: usize,
    style: StyleMetrics,
    bonus: f64,
}

/// Оценка отжига/генетики: золото минус штраф за попытки стен плюс бонус за аккуратность.
fn judge(gold: f64, sim: &Simulator, plan: &[PlanStep], budget: WallBudget, cfg: &OptimizerConfig) -> Eval {
    let walls_used = wall_attempts(sim, plan, budget.base);
    let style = style_metrics(sim, cfg.style.row_target);
    let bonus = style.bonus(&cfg.style);
    Eval {
        gold,
        score: gold - cfg.wall_penalty.max(0.0) * walls_used as f64 + bonus,
        walls_used,
        style,
        bonus,
    }
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
    sim.calculate_worker_distances_fast();

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
    plan: &[PlanStep],
    budget: WallBudget,
    style_on: bool,
) -> Option<(Simulator, Vec<Change>)> {
    let mut sim = base.clone();
    sim.set_defer_path_calculation(true);
    let mut changes = Vec::new();
    let roll = rng.random_range(0..100);
    // Границы долей: relocate | wall_row | snap | swap. Без стен их доля уходит перемещениям,
    // без весов стиля snap не используется (тогда как раньше: 45 / 35 / 20).
    let (relocate_to, wall_to, snap_to) = match (budget.max > 0, style_on) {
        (true, true) => (30, 60, 80),
        (true, false) => (45, 80, 80),
        (false, true) => (60, 60, 80),
        (false, false) => (80, 80, 80),
    };
    let done = if roll < relocate_to {
        relocate(&mut sim, rng, progress, &mut changes)
    } else if roll < wall_to {
        wall_row(&mut sim, rng, progress, plan, budget, &mut changes)
    } else if roll < snap_to {
        snap(&mut sim, rng, &mut changes)
    } else {
        swap(&mut sim, rng, &mut changes)
    };
    done.map(|_| (sim, changes))
}

/// Прижать здание вплотную к другому (с любой из 8 сторон, включая углы), чтобы получались
/// ряды, столбцы и диагонали.
fn snap(sim: &mut Simulator, rng: &mut impl Rng, changes: &mut Vec<Change>) -> Option<()> {
    let list = movable(sim);
    let &(id, t, ox, oy) = list.choose(rng)?;
    let fp = Footprint::for_type(t);
    let (w, h) = (fp.width() as i32, fp.height() as i32);
    let targets: Vec<(i32, i32, i32, i32)> = sim
        .buildings()
        .iter()
        .filter(|b| b.id != id && b.building_type != BuildingType::Stockpile)
        .map(|b| (b.x, b.y, b.width() as i32, b.height() as i32))
        .collect();
    let &(tx, ty, tw, th) = targets.choose(rng)?;
    let xs = [tx - w, tx, tx + tw];
    let ys = [ty - h, ty, ty + th];
    let (x, y) = (*xs.choose(rng)?, *ys.choose(rng)?);
    if (x, y) == (tx, ty) || (x, y) == (ox, oy) {
        return None;
    }
    let bounds = sim.map_bounds();
    if x < bounds.min_x + MARGIN
        || y < bounds.min_y + MARGIN
        || x + w > bounds.max_x - MARGIN
        || y + h > bounds.max_y - MARGIN
    {
        return None;
    }
    if !sim.remove_building(id) {
        return None;
    }
    changes.push(Change::Removed { x: ox, y: oy });
    sim.place_building(t, x, y).ok()?;
    changes.push(Change::Placed(PlanStep {
        building_type: t,
        x,
        y,
        wall: None,
    }));
    Some(())
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
                wall: None,
            }));
            return Some(());
        }
    }
    None
}

/// Прямоугольник w x h свободен и целиком внутри карты.
fn area_free(sim: &Simulator, x: i32, y: i32, w: i32, h: i32) -> bool {
    let bounds = sim.map_bounds();
    (x..x + w).all(|cx| {
        (y..y + h).all(|cy| bounds.contains(cx, cy) && !sim.is_cell_occupied(cx, cy))
    })
}

/// Одна стена вплотную к ряду мастерских (все стоят впритык друг к другу) -> мастерские ставятся ->
/// стена убирается (или остаётся). Все мастерские ряда получают повёрнутый вход от одной стены.
/// Стороны соответствуют workshop_wall_rotation_steps: top->0, right->1, bottom->2, left->3.
/// Создание стены = 1 попытка, независимо от длины и числа мастерских.
fn wall_row(
    sim: &mut Simulator,
    rng: &mut impl Rng,
    progress: f64,
    plan: &[PlanStep],
    budget: WallBudget,
    changes: &mut Vec<Change>,
) -> Option<()> {
    let workshops: Vec<_> = movable(sim)
        .into_iter()
        .filter(|m| is_workshop(m.1))
        .collect();
    if workshops.is_empty() {
        return None;
    }

    // Какие мастерские разворачиваем одной стеной.
    let mut chosen: Vec<(u32, BuildingType, i32, i32)> = Vec::new();
    let groups: Vec<u32> = removed_groups(plan.iter()).into_iter().collect();
    if !groups.is_empty() && rng.random_bool(0.5) {
        // Переукладываем существующий ряд целиком и пробуем нарастить его.
        let g = *groups.choose(rng)?;
        for w in &workshops {
            let in_group = plan
                .iter()
                .any(|s| (s.x, s.y) == (w.2, w.3) && s.wall.as_ref().is_some_and(|p| p.group == g));
            if in_group {
                chosen.push(*w);
            }
        }
        for _ in 0..rng.random_range(0..=2) {
            if chosen.len() >= MAX_ROW {
                break;
            }
            if let Some(w) = workshops.choose(rng) {
                if !chosen.iter().any(|c| c.0 == w.0) {
                    chosen.push(*w);
                }
            }
        }
    } else {
        // Новый ряд: размер смещён в сторону больших рядов.
        let cap = workshops.len().min(MAX_ROW);
        let k = rng.random_range(1..=cap).max(rng.random_range(1..=cap));
        let mut pool = workshops.clone();
        pool.shuffle(rng);
        chosen = pool.into_iter().take(k).collect();
    }
    if chosen.is_empty() {
        return None;
    }
    chosen.shuffle(rng);

    // Бюджет попыток: оставленные стены + группы со снятой стеной, которые останутся
    // после переукладки выбранных мастерских, + одна новая.
    let chosen_pos: HashSet<(i32, i32)> = chosen.iter().map(|c| (c.2, c.3)).collect();
    let kept_extra = sim.walls().len().saturating_sub(budget.base);
    let remaining =
        removed_groups(plan.iter().filter(|s| !chosen_pos.contains(&(s.x, s.y)))).len();
    if kept_extra + remaining + 1 > budget.max {
        return None;
    }
    let group = plan
        .iter()
        .filter_map(|s| s.wall.as_ref().map(|w| w.group))
        .max()
        .map_or(1, |g| g + 1);

    let first = chosen[0];
    let (around, radius) = pick_target(sim, rng, first.0, (first.2, first.3), progress)?;
    for c in &chosen {
        if !sim.remove_building(c.0) {
            return None;
        }
        changes.push(Change::Removed { x: c.2, y: c.3 });
    }

    let k = chosen.len() as i32;
    for _ in 0..30 {
        let (x, y) = pick_position(sim, rng, first.1, around, radius)?;
        // rot 0 (стена сверху) вход не меняет: он и так снизу. Не тратим на неё попытку.
        let rot = rng.random_range(1..4usize);
        // Ряд лежит вдоль стены: top/bottom — горизонтально, right/left — вертикально.
        let (w, h) = if rot % 2 == 0 { (N * k, N) } else { (N, N * k) };
        let (sx, sy, ex, ey) = match rot {
            0 => (x, y + N, x + w - 1, y + N),
            1 => (x + N, y, x + N, y + h - 1),
            2 => (x, y - 1, x + w - 1, y - 1),
            _ => (x - 1, y, x - 1, y + h - 1),
        };
        let bounds = sim.map_bounds();
        let (lo_x, hi_x) = (x.min(sx), (x + w).max(ex + 1));
        let (lo_y, hi_y) = (y.min(sy), (y + h).max(ey + 1));
        if lo_x < bounds.min_x + MARGIN
            || hi_x > bounds.max_x - MARGIN
            || lo_y < bounds.min_y + MARGIN
            || hi_y > bounds.max_y - MARGIN
        {
            continue;
        }
        let slots: Vec<(i32, i32)> = (0..k)
            .map(|i| if rot % 2 == 0 { (x + N * i, y) } else { (x, y + N * i) })
            .collect();
        let wall_free = line_cells(sx, sy, ex, ey)
            .iter()
            .all(|&(cx, cy)| !sim.is_cell_occupied(cx, cy));
        if !wall_free || !slots.iter().all(|&(px, py)| area_free(sim, px, py, N, N)) {
            continue;
        }

        let mut trial = sim.clone();
        if trial.place_wall(sx, sy, ex, ey).is_err() {
            continue;
        }
        if !chosen
            .iter()
            .zip(&slots)
            .all(|(c, &(px, py))| trial.place_building(c.1, px, py).is_ok())
        {
            continue;
        }
        // Часть стен оставляем: снимать её необязательно.
        let kept = rng.random_bool(0.2);
        if !kept {
            // клетки были свободны, поэтому последняя стена в этой клетке — наша
            trial.remove_whole_wall_at(sx, sy);
        }
        *sim = trial;
        for (c, &(px, py)) in chosen.iter().zip(&slots) {
            changes.push(Change::Placed(PlanStep {
                building_type: c.1,
                x: px,
                y: py,
                wall: Some(WallPlan {
                    group,
                    side: SIDES[rot].to_string(),
                    sx,
                    sy,
                    ex,
                    ey,
                    kept,
                }),
            }));
        }
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
            wall: None,
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
    let style_on = sh.cfg.style.is_active();
    let budget = WallBudget {
        max: sh.cfg.max_walls,
        base: start.walls().len(),
    };

    let mut cur = start;
    cur.set_defer_path_calculation(true);
    cur.disable_build_log();

    let mut plan: Vec<PlanStep> = movable(&cur)
        .into_iter()
        .map(|(_, t, x, y)| PlanStep {
            building_type: t,
            x,
            y,
            wall: None,
        })
        .collect();

    // Потоки кроме первого стартуют из «перемешанной» расстановки — для разнообразия.
    if id > 0 {
        for _ in 0..plan.len() * 3 {
            if sh.cancel.load(Ordering::Relaxed) {
                return;
            }
            if let Some((s, ch)) = mutate(&cur, &mut rng, 0.0, &plan, budget, style_on) {
                cur = s;
                apply_changes(&mut plan, ch);
            }
        }
    }

    let gold = evaluate(&mut cur, settings);
    let first = judge(gold, &cur, &plan, budget, &sh.cfg);
    sh.evaluated.fetch_add(1, Ordering::Relaxed);
    let mut cur_score = first.score;
    let mut best_score = cur_score;
    let mut best = (cur.clone(), plan.clone());
    sh.offer(id, 0, first, &best.0, &best.1);

    let patience = (iters / 8).max(300);
    let mut last_improve = 0usize;

    for i in 0..iters {
        if sh.cancel.load(Ordering::Relaxed) {
            break;
        }
        let progress = i as f64 / iters as f64;
        let temp = t0 * (t1 / t0).powf(progress);

        if let Some((mut cand, changes)) =
            mutate(&cur, &mut rng, progress, &plan, budget, style_on)
        {
            let gold = evaluate(&mut cand, settings);
            let mut cand_plan = plan.clone();
            apply_changes(&mut cand_plan, changes);
            let eval = judge(gold, &cand, &cand_plan, budget, &sh.cfg);
            sh.evaluated.fetch_add(1, Ordering::Relaxed);
            let delta = eval.score - cur_score;
            if delta >= 0.0 || rng.random::<f64>() < (delta / temp).exp() {
                cur = cand;
                plan = cand_plan;
                cur_score = eval.score;
                if eval.score > best_score + 1e-9 {
                    best_score = eval.score;
                    best = (cur.clone(), plan.clone());
                    last_improve = i;
                    sh.offer(id, i, eval, &best.0, &best.1);
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
        "Старт ({:?}): {} потоков x {} итераций, подвижных зданий: {}, top {}, попыток стены: {}, стиль: align {} rows {} diag {} sym {} compact {}",
        cfg.algorithm,
        threads,
        cfg.max_iterations,
        movable(&layout).len(),
        cfg.top_n,
        cfg.max_walls,
        cfg.style.align,
        cfg.style.rows,
        cfg.style.diagonal,
        cfg.style.symmetry,
        cfg.style.compact
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
        fixed_steps: fixed_steps(&layout),
    });

    let algorithm = shared.cfg.algorithm;
    let mut handles = Vec::new();
    for worker in 0..threads {
        let sh = Arc::clone(&shared);
        let start = layout.clone();
        handles.push(
            thread::Builder::new()
                .name(format!("optimizer-{worker}"))
                .spawn(move || match algorithm {
                    OptimizerAlgorithm::Annealing => run_worker(worker, start, sh),
                    OptimizerAlgorithm::Genetic => genetic::run_genetic(worker, start, sh),
                })
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