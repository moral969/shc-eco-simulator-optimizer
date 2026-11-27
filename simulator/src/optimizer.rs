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
    Simulator, StockpileResource, walls::line_cells,
};

/// Отступ от края карты, при котором place_building/place_wall не расширяют карту.
const MARGIN: i32 = 55;
/// Сторона мастерской.
const N: i32 = 4;
/// Максимум мастерских в одном ряду под одной стеной.
const MAX_ROW: usize = 8;
const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

#[derive(Debug, Clone)]
pub struct OptimizerConfig {
    /// Итераций на один поток.
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
    /// Вокруг склада (включая диагонали) остаётся свободная клетка: здания вплотную к складу не ставятся.
    pub stockpile_clearance: bool,
    /// Соприкасавшиеся на исходной карте здания — неделимые группы (двигаются и вращаются целиком).
    pub keep_groups: bool,
    /// Максимум зданий в кластере из слипшихся исходных групп. 0 — без ограничения.
    /// Не запрещает двигать исходную группу, даже если она сама больше лимита.
    pub max_cluster_size: usize,
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
            stockpile_clearance: true,
            keep_groups: true,
            max_cluster_size: 8,
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
    #[allow(clippy::too_many_arguments)]
    fn offer(
        &self,
        worker: usize,
        iteration: usize,
        gold: f64,
        score: f64,
        walls_used: usize,
        sim: &Simulator,
        plan: &[PlanStep],
    ) {
        {
            let a = self.archive.lock().unwrap_or_else(|e| e.into_inner());
            if score <= a.threshold() {
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
                gold_per_minute: gold,
                score,
                walls_used,
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
        // hi — последняя клетка; размер берём по типу здания (в группе бывают не только 4x4).
        let (lo, hi) = idx.iter().fold((i32::MAX, i32::MIN), |(lo, hi), &i| {
            let v = if horizontal { out[i].x } else { out[i].y };
            let s = Footprint::for_type(out[i].building_type).width() as i32;
            (lo.min(v), hi.max(v + s - 1))
        });
        for &i in &idx {
            if let Some(w) = out[i].wall.as_mut() {
                if horizontal {
                    w.sx = lo;
                    w.ex = hi;
                } else {
                    w.sy = lo;
                    w.ey = hi;
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

/// Оценка отжига: золото минус штраф за использованные попытки стен.
fn energy(gold: f64, sim: &Simulator, plan: &[PlanStep], budget: WallBudget, penalty: f64) -> (f64, usize) {
    let used = wall_attempts(sim, plan, budget.base);
    (gold - penalty * used as f64, used)
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

// ---------------------------------------------------------------- groups & rules

/// Ограничения, которые учитываются при генерации вариантов.
#[derive(Debug, Clone, Copy)]
struct Rules {
    /// Вокруг склада (включая диагонали) должна оставаться свободная клетка.
    clearance: bool,
    /// Максимум зданий в кластере из нескольких исходных групп (0 — без ограничения).
    max_cluster: usize,
}

/// Неделимая группа: здания, соприкасавшиеся на исходной карте.
/// Двигается и вращается только целиком (ID зданий при переносе меняются, поэтому храним координаты).
#[derive(Debug, Clone)]
struct Group {
    /// (тип, x, y) каждого здания: текущий левый нижний угол.
    members: Vec<(BuildingType, i32, i32)>,
}

/// Прямоугольник здания: (x0, y0, x1, y1), x1/y1 не включительно.
fn rect_of(t: BuildingType, x: i32, y: i32) -> (i32, i32, i32, i32) {
    let fp = Footprint::for_type(t);
    (x, y, x + fp.width() as i32, y + fp.height() as i32)
}

/// Прямоугольники соприкасаются сторонами (общий отрезок границы). Касание одним углом не считается.
fn rects_touch(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
    ((a.2 == b.0 || b.2 == a.0) && a.1 < b.3 && b.1 < a.3)
        || ((a.3 == b.1 || b.3 == a.1) && a.0 < b.2 && b.0 < a.2)
}

/// Между прямоугольниками нет свободной клетки: касание сторонами, углом или пересечение.
fn rects_near(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
    a.0 <= b.2 && b.0 <= a.2 && a.1 <= b.3 && b.1 <= a.3
}

/// Исходные группы: компоненты связности подвижных зданий.
/// При keep_groups = false каждое здание — отдельная группа (прежнее поведение).
fn initial_groups(sim: &Simulator, keep_groups: bool) -> Vec<Group> {
    let list = movable(sim);
    let rects: Vec<_> = list.iter().map(|m| rect_of(m.1, m.2, m.3)).collect();
    let mut seen = vec![false; list.len()];
    let mut groups = Vec::new();
    for start in 0..list.len() {
        if seen[start] {
            continue;
        }
        seen[start] = true;
        let mut stack = vec![start];
        let mut members = Vec::new();
        while let Some(i) = stack.pop() {
            members.push((list[i].1, list[i].2, list[i].3));
            for j in 0..list.len() {
                if keep_groups && !seen[j] && rects_touch(rects[i], rects[j]) {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
        groups.push(Group { members });
    }
    groups
}

/// Номер группы, в которую входит здание с левым нижним углом (x, y).
fn group_index(groups: &[Group], x: i32, y: i32) -> Option<usize> {
    groups
        .iter()
        .position(|g| g.members.iter().any(|m| (m.1, m.2) == (x, y)))
}

/// Подвижные здания из одиночных групп (их можно двигать по одному).
fn free_movable(sim: &Simulator, groups: &[Group]) -> Vec<(u32, BuildingType, i32, i32)> {
    movable(sim)
        .into_iter()
        .filter(|m| {
            group_index(groups, m.2, m.3).is_some_and(|g| groups[g].members.len() == 1)
        })
        .collect()
}

/// Здание не стоит вплотную (в том числе по диагонали) к складу.
fn clear_of_stockpiles(sim: &Simulator, t: BuildingType, x: i32, y: i32) -> bool {
    rect_clear_of_stockpiles(sim, rect_of(t, x, y))
}

/// Прямоугольник не стоит вплотную (в том числе по диагонали) к складу.
fn rect_clear_of_stockpiles(sim: &Simulator, rect: (i32, i32, i32, i32)) -> bool {
    !sim.buildings().iter().any(|s| {
        s.building_type == BuildingType::Stockpile
            && rects_near(
                rect,
                (s.x, s.y, s.x + s.width() as i32, s.y + s.height() as i32),
            )
    })
}

/// Допустимо ли поставить здание t в (x, y) с точки зрения свободной зоны вокруг складов.
fn placement_ok(sim: &Simulator, rules: Rules, t: BuildingType, x: i32, y: i32) -> bool {
    !rules.clearance || clear_of_stockpiles(sim, t, x, y)
}

/// Допустимо ли поставить стену (sx, sy)-(ex, ey): она тоже не стоит вплотную к складу.
fn wall_ok(sim: &Simulator, rules: Rules, sx: i32, sy: i32, ex: i32, ey: i32) -> bool {
    !rules.clearance
        || rect_clear_of_stockpiles(sim, (sx.min(ex), sy.min(ey), sx.max(ex) + 1, sy.max(ey) + 1))
}

/// Кластер — группы, соприкасающиеся зданиями. Объединение нескольких исходных групп допустимо,
/// только если в кластере не больше max_cluster зданий. Одиночную группу лимит не ограничивает.
/// touched — группы, чьё положение изменилось (остальные кластеры не менялись).
fn clusters_ok(groups: &[Group], touched: &[usize], rules: Rules) -> bool {
    if rules.max_cluster == 0 {
        return true;
    }
    let rects: Vec<Vec<_>> = groups
        .iter()
        .map(|g| g.members.iter().map(|m| rect_of(m.0, m.1, m.2)).collect())
        .collect();
    let touch = |a: usize, b: usize| {
        rects[a]
            .iter()
            .any(|ra| rects[b].iter().any(|rb| rects_touch(*ra, *rb)))
    };
    for &start in touched {
        let mut seen = vec![false; groups.len()];
        seen[start] = true;
        let mut stack = vec![start];
        let (mut count, mut size) = (0usize, 0usize);
        while let Some(g) = stack.pop() {
            count += 1;
            size += groups[g].members.len();
            for other in 0..groups.len() {
                if !seen[other] && touch(g, other) {
                    seen[other] = true;
                    stack.push(other);
                }
            }
        }
        if count > 1 && size > rules.max_cluster {
            return false;
        }
    }
    true
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
    /// Новый состав группы с номером index (после переноса/поворота/обмена).
    Group {
        index: usize,
        members: Vec<(BuildingType, i32, i32)>,
    },
}

fn apply_changes(plan: &mut Vec<PlanStep>, groups: &mut [Group], changes: Vec<Change>) {
    for change in changes {
        match change {
            Change::Removed { x, y } => plan.retain(|s| (s.x, s.y) != (x, y)),
            Change::Placed(step) => plan.push(step),
            Change::Group { index, members } => groups[index].members = members,
        }
    }
}

fn mutate(
    base: &Simulator,
    rng: &mut impl Rng,
    progress: f64,
    plan: &[PlanStep],
    groups: &[Group],
    rules: Rules,
    budget: WallBudget,
) -> Option<(Simulator, Vec<Change>)> {
    let mut sim = base.clone();
    sim.set_defer_path_calculation(true);
    let mut changes = Vec::new();
    let roll = rng.random_range(0..100);
    // При max == 0 долю стен отдаём перемещениям, чтобы не гонять пустые итерации.
    let done = if roll < 45 || (budget.max == 0 && roll < 80) {
        // Часть перемещений достаётся целым многозвенным группам.
        if groups.iter().any(|g| g.members.len() > 1) && rng.random_bool(0.4) {
            move_group(&mut sim, rng, progress, groups, rules, &mut changes)
        } else {
            relocate(&mut sim, rng, progress, groups, rules, &mut changes)
        }
    } else if roll < 80 {
        wall_row(&mut sim, rng, progress, plan, groups, rules, budget, &mut changes)
    } else {
        swap(&mut sim, rng, groups, &mut changes)
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
    let fp = Footprint::for_type(t);
    pick_position_rect(sim, rng, fp.width() as i32, fp.height() as i32, around, radius)
}

/// Левый нижний угол прямоугольника w x h рядом с around (без расширения карты).
fn pick_position_rect(
    sim: &Simulator,
    rng: &mut impl Rng,
    w: i32,
    h: i32,
    around: (i32, i32),
    radius: i32,
) -> Option<(i32, i32)> {
    let bounds = sim.map_bounds();
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
    groups: &[Group],
    rules: Rules,
    changes: &mut Vec<Change>,
) -> Option<()> {
    // По одному двигаем только одиночные группы; многозвенные переносит move_group.
    let list = free_movable(sim, groups);
    let &(id, t, ox, oy) = list.choose(rng)?;
    let gi = group_index(groups, ox, oy)?;
    let (around, radius) = pick_target(sim, rng, id, (ox, oy), progress)?;
    if !sim.remove_building(id) {
        return None;
    }
    changes.push(Change::Removed { x: ox, y: oy });
    let mut trial = groups.to_vec();
    for _ in 0..30 {
        let (x, y) = pick_position(sim, rng, t, around, radius)?;
        if (x, y) == (ox, oy) {
            continue;
        }
        // Недопустимые места (зазор у склада, лимит кластера) отбрасываем до постановки.
        if !placement_ok(sim, rules, t, x, y) {
            continue;
        }
        trial[gi].members = vec![(t, x, y)];
        if !clusters_ok(&trial, &[gi], rules) {
            continue;
        }
        if sim.place_building(t, x, y).is_ok() {
            changes.push(Change::Placed(PlanStep {
                building_type: t,
                x,
                y,
                wall: None,
            }));
            changes.push(Change::Group {
                index: gi,
                members: vec![(t, x, y)],
            });
            return Some(());
        }
    }
    None
}

/// Перенос и поворот (0/90/180/270°) целой исходной группы: форма и взаимное расположение сохраняются.
fn move_group(
    sim: &mut Simulator,
    rng: &mut impl Rng,
    progress: f64,
    groups: &[Group],
    rules: Rules,
    changes: &mut Vec<Change>,
) -> Option<()> {
    let multi: Vec<usize> = (0..groups.len())
        .filter(|&i| groups[i].members.len() > 1)
        .collect();
    let gi = *multi.choose(rng)?;
    let members = &groups[gi].members;
    // Ферма не вращается: у неё фиксированные вход и раскладка поля.
    let rot = if members.iter().any(|m| m.0 == BuildingType::WheatFarm) {
        0
    } else {
        rng.random_range(0..4usize)
    };
    let rotated: Vec<(BuildingType, i32, i32)> = members
        .iter()
        .map(|&(t, x, y)| {
            let s = Footprint::for_type(t).width() as i32;
            let (rx, ry) = match rot {
                0 => (x, y),
                1 => (-y - s + 1, x),
                2 => (-x - s + 1, -y - s + 1),
                _ => (y, -x - s + 1),
            };
            (t, rx, ry)
        })
        .collect();
    let min_x = rotated.iter().map(|m| m.1).min()?;
    let min_y = rotated.iter().map(|m| m.2).min()?;
    let max_x = rotated.iter().map(|m| rect_of(m.0, m.1, m.2).2).max()?;
    let max_y = rotated.iter().map(|m| rect_of(m.0, m.1, m.2).3).max()?;
    let (w, h) = (max_x - min_x, max_y - min_y);

    let ids = members
        .iter()
        .map(|&(_, x, y)| sim.buildings().iter().find(|b| (b.x, b.y) == (x, y)).map(|b| b.id))
        .collect::<Option<Vec<u32>>>()?;
    let old = (members[0].1, members[0].2);
    let (around, radius) = pick_target(sim, rng, ids[0], old, progress)?;
    for (&id, m) in ids.iter().zip(members) {
        if !sim.remove_building(id) {
            return None;
        }
        changes.push(Change::Removed { x: m.1, y: m.2 });
    }

    for _ in 0..10 {
        let (bx, by) = pick_position_rect(sim, rng, w, h, around, radius)?;
        let new_members: Vec<(BuildingType, i32, i32)> = rotated
            .iter()
            .map(|&(t, x, y)| (t, x - min_x + bx, y - min_y + by))
            .collect();
        if new_members == *members {
            continue;
        }
        let fits = new_members.iter().all(|&(t, x, y)| {
            let r = rect_of(t, x, y);
            placement_ok(sim, rules, t, x, y) && area_free(sim, r.0, r.1, r.2 - r.0, r.3 - r.1)
        });
        if !fits {
            continue;
        }
        let mut trial = groups.to_vec();
        trial[gi].members = new_members.clone();
        if !clusters_ok(&trial, &[gi], rules) {
            continue;
        }
        for &(t, x, y) in &new_members {
            sim.place_building(t, x, y).ok()?;
            changes.push(Change::Placed(PlanStep {
                building_type: t,
                x,
                y,
                wall: None,
            }));
        }
        changes.push(Change::Group {
            index: gi,
            members: new_members,
        });
        return Some(());
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
#[allow(clippy::too_many_arguments)]
fn wall_row(
    sim: &mut Simulator,
    rng: &mut impl Rng,
    progress: f64,
    plan: &[PlanStep],
    groups: &[Group],
    rules: Rules,
    budget: WallBudget,
    changes: &mut Vec<Change>,
) -> Option<()> {
    // В ряд попадают только одиночные группы: здания многозвенных групп разбирать нельзя.
    let workshops: Vec<_> = free_movable(sim, groups)
        .into_iter()
        .filter(|m| is_workshop(m.1))
        .collect();
    // Многозвенную группу с мастерской разворачиваем стеной целиком (одна попытка стены на группу).
    let has_group = groups
        .iter()
        .any(|g| g.members.len() > 1 && g.members.iter().any(|m| is_workshop(m.0)));
    if has_group && (workshops.is_empty() || rng.random_bool(0.4)) {
        return wall_group(sim, rng, progress, plan, groups, rules, budget, changes);
    }
    if workshops.is_empty() {
        return None;
    }
    // Ряд вплотную стоящих одиночных групп — кластер, его размер ограничен max_cluster.
    let row_cap = if rules.max_cluster == 0 {
        MAX_ROW
    } else {
        MAX_ROW.min(rules.max_cluster)
    };

    // Какие мастерские разворачиваем одной стеной.
    let mut chosen: Vec<(u32, BuildingType, i32, i32)> = Vec::new();
    let wall_groups: Vec<u32> = removed_groups(plan.iter()).into_iter().collect();
    if !wall_groups.is_empty() && rng.random_bool(0.5) {
        // Переукладываем существующий ряд целиком и пробуем нарастить его.
        let g = *wall_groups.choose(rng)?;
        for w in &workshops {
            let in_group = plan
                .iter()
                .any(|s| (s.x, s.y) == (w.2, w.3) && s.wall.as_ref().is_some_and(|p| p.group == g));
            if in_group {
                chosen.push(*w);
            }
        }
        for _ in 0..rng.random_range(0..=2) {
            if chosen.len() >= row_cap {
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
        let cap = workshops.len().min(row_cap);
        let k = rng.random_range(1..=cap).max(rng.random_range(1..=cap));
        let mut pool = workshops.clone();
        pool.shuffle(rng);
        chosen = pool.into_iter().take(k).collect();
    }
    if chosen.is_empty() {
        return None;
    }
    chosen.shuffle(rng);
    // Номера исходных групп выбранных мастерских (по старым координатам, до удаления).
    let gidx = chosen
        .iter()
        .map(|c| group_index(groups, c.2, c.3))
        .collect::<Option<Vec<usize>>>()?;

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
        let rot = rng.random_range(0..4usize);
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
            .all(|&(cx, cy)| !sim.is_cell_occupied(cx, cy))
            && wall_ok(sim, rules, sx, sy, ex, ey);
        if !wall_free || !slots.iter().all(|&(px, py)| area_free(sim, px, py, N, N)) {
            continue;
        }
        // Зазор у складов и лимит кластера проверяем до постройки: недопустимые ряды не генерируем.
        if !chosen
            .iter()
            .zip(&slots)
            .all(|(c, &(px, py))| placement_ok(sim, rules, c.1, px, py))
        {
            continue;
        }
        let mut trial_groups = groups.to_vec();
        for (&g, (c, &(px, py))) in gidx.iter().zip(chosen.iter().zip(&slots)) {
            trial_groups[g].members = vec![(c.1, px, py)];
        }
        if !clusters_ok(&trial_groups, &gidx, rules) {
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
        for (i, (c, &(px, py))) in chosen.iter().zip(&slots).enumerate() {
            changes.push(Change::Group {
                index: gidx[i],
                members: vec![(c.1, px, py)],
            });
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

/// Поворот группы на rot * 90°: форма и взаимное расположение зданий сохраняются.
fn rotate_members(
    members: &[(BuildingType, i32, i32)],
    rot: usize,
) -> Vec<(BuildingType, i32, i32)> {
    members
        .iter()
        .map(|&(t, x, y)| {
            let s = Footprint::for_type(t).width() as i32;
            let (rx, ry) = match rot {
                0 => (x, y),
                1 => (-y - s + 1, x),
                2 => (-x - s + 1, -y - s + 1),
                _ => (y, -x - s + 1),
            };
            (t, rx, ry)
        })
        .collect()
}

/// wall_row для многозвенной группы: группа переносится и вращается целиком, одна стена ставится
/// вплотную к стороне её габарита и разворачивает входы мастерских, стоящих на этой стороне.
/// Стороны те же, что в wall_row: 0 — сверху, 1 — справа, 2 — снизу, 3 — слева.
#[allow(clippy::too_many_arguments)]
fn wall_group(
    sim: &mut Simulator,
    rng: &mut impl Rng,
    progress: f64,
    plan: &[PlanStep],
    groups: &[Group],
    rules: Rules,
    budget: WallBudget,
    changes: &mut Vec<Change>,
) -> Option<()> {
    // Берём группу, в которой есть мастерская: иначе стена ничего не развернёт.
    let candidates: Vec<usize> = (0..groups.len())
        .filter(|&i| {
            groups[i].members.len() > 1 && groups[i].members.iter().any(|m| is_workshop(m.0))
        })
        .collect();
    let gi = *candidates.choose(rng)?;
    let members = &groups[gi].members;

    // Бюджет попыток: как в wall_row (группа переукладывается целиком + одна новая стена).
    let member_pos: HashSet<(i32, i32)> = members.iter().map(|m| (m.1, m.2)).collect();
    let kept_extra = sim.walls().len().saturating_sub(budget.base);
    let remaining =
        removed_groups(plan.iter().filter(|s| !member_pos.contains(&(s.x, s.y)))).len();
    if kept_extra + remaining + 1 > budget.max {
        return None;
    }
    let group = plan
        .iter()
        .filter_map(|s| s.wall.as_ref().map(|w| w.group))
        .max()
        .map_or(1, |g| g + 1);

    // Ферма не вращается: у неё фиксированные вход и раскладка поля.
    let rot = if members.iter().any(|m| m.0 == BuildingType::WheatFarm) {
        0
    } else {
        rng.random_range(0..4usize)
    };
    let rotated = rotate_members(members, rot);
    let min_x = rotated.iter().map(|m| m.1).min()?;
    let min_y = rotated.iter().map(|m| m.2).min()?;
    let max_x = rotated.iter().map(|m| rect_of(m.0, m.1, m.2).2).max()?;
    let max_y = rotated.iter().map(|m| rect_of(m.0, m.1, m.2).3).max()?;
    let (w, h) = (max_x - min_x, max_y - min_y);

    let ids = members
        .iter()
        .map(|&(_, x, y)| sim.buildings().iter().find(|b| (b.x, b.y) == (x, y)).map(|b| b.id))
        .collect::<Option<Vec<u32>>>()?;
    let old = (members[0].1, members[0].2);
    let (around, radius) = pick_target(sim, rng, ids[0], old, progress)?;
    for (&id, m) in ids.iter().zip(members) {
        if !sim.remove_building(id) {
            return None;
        }
        changes.push(Change::Removed { x: m.1, y: m.2 });
    }

    for _ in 0..30 {
        let (bx, by) = pick_position_rect(sim, rng, w, h, around, radius)?;
        let side = rng.random_range(0..4usize);
        // Стена вплотную к стороне габарита группы (до MARGIN хватает: карта не расширяется).
        let (sx, sy, ex, ey) = match side {
            0 => (bx, by + h, bx + w - 1, by + h),
            1 => (bx + w, by, bx + w, by + h - 1),
            2 => (bx, by - 1, bx + w - 1, by - 1),
            _ => (bx - 1, by, bx - 1, by + h - 1),
        };
        let new_members: Vec<(BuildingType, i32, i32)> = rotated
            .iter()
            .map(|&(t, x, y)| (t, x - min_x + bx, y - min_y + by))
            .collect();
        let fits = new_members.iter().all(|&(t, x, y)| {
            let r = rect_of(t, x, y);
            placement_ok(sim, rules, t, x, y) && area_free(sim, r.0, r.1, r.2 - r.0, r.3 - r.1)
        });
        let wall_free = line_cells(sx, sy, ex, ey)
            .iter()
            .all(|&(cx, cy)| !sim.is_cell_occupied(cx, cy))
            && wall_ok(sim, rules, sx, sy, ex, ey);
        if !fits || !wall_free {
            continue;
        }
        let mut trial_groups = groups.to_vec();
        trial_groups[gi].members = new_members.clone();
        if !clusters_ok(&trial_groups, &[gi], rules) {
            continue;
        }

        let mut trial = sim.clone();
        if trial.place_wall(sx, sy, ex, ey).is_err() {
            continue;
        }
        if !new_members
            .iter()
            .all(|&(t, x, y)| trial.place_building(t, x, y).is_ok())
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
        changes.push(Change::Group {
            index: gi,
            members: new_members.clone(),
        });
        for &(t, x, y) in &new_members {
            changes.push(Change::Placed(PlanStep {
                building_type: t,
                x,
                y,
                wall: Some(WallPlan {
                    group,
                    side: SIDES[side].to_string(),
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
fn swap(
    sim: &mut Simulator,
    rng: &mut impl Rng,
    groups: &[Group],
    changes: &mut Vec<Change>,
) -> Option<()> {
    // Меняем местами только одиночные группы. Геометрия (а значит зазор у складов и кластеры)
    // не меняется: оба здания 4x4 просто обмениваются клетками.
    let list: Vec<_> = free_movable(sim, groups)
        .into_iter()
        .filter(|m| m.1 == BuildingType::Armoury || is_workshop(m.1))
        .collect();
    let &(ia, ta, xa, ya) = list.choose(rng)?;
    let &(ib, tb, xb, yb) = list.choose(rng)?;
    if ta == tb {
        return None;
    }
    let ga = group_index(groups, xa, ya)?;
    let gb = group_index(groups, xb, yb)?;
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
    changes.push(Change::Group {
        index: ga,
        members: vec![(ta, xb, yb)],
    });
    changes.push(Change::Group {
        index: gb,
        members: vec![(tb, xa, ya)],
    });
    Some(())
}

// ---------------------------------------------------------------- worker

fn run_worker(id: usize, start: Simulator, sh: Arc<Shared>) {
    let mut rng = rand::rng();
    let settings = sh.settings;
    let iters = sh.cfg.max_iterations.max(1);
    let t0 = sh.cfg.initial_temperature.max(1e-6);
    let t1 = sh.cfg.final_temperature.max(1e-6).min(t0);
    let penalty = sh.cfg.wall_penalty.max(0.0);
    let budget = WallBudget {
        max: sh.cfg.max_walls,
        base: start.walls().len(),
    };
    let rules = Rules {
        clearance: sh.cfg.stockpile_clearance,
        // Без режима групп лимит кластера не нужен: каждое здание — своя группа.
        max_cluster: if sh.cfg.keep_groups {
            sh.cfg.max_cluster_size
        } else {
            0
        },
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
    // Исходные группы считаем до перемешивания: дальше они только двигаются целиком.
    let mut groups = initial_groups(&cur, sh.cfg.keep_groups);

    // Потоки кроме первого стартуют из «перемешанной» расстановки — для разнообразия.
    if id > 0 {
        for _ in 0..plan.len() * 3 {
            if sh.cancel.load(Ordering::Relaxed) {
                return;
            }
            if let Some((s, ch)) = mutate(&cur, &mut rng, 0.0, &plan, &groups, rules, budget) {
                cur = s;
                apply_changes(&mut plan, &mut groups, ch);
            }
        }
    }

    let gold = evaluate(&mut cur, settings);
    let (mut cur_score, used) = energy(gold, &cur, &plan, budget, penalty);
    sh.evaluated.fetch_add(1, Ordering::Relaxed);
    let mut best_score = cur_score;
    let mut best = (cur.clone(), plan.clone(), groups.clone());
    sh.offer(id, 0, gold, best_score, used, &best.0, &best.1);

    let patience = (iters / 8).max(300);
    let mut last_improve = 0usize;

    for i in 0..iters {
        if sh.cancel.load(Ordering::Relaxed) {
            break;
        }
        let progress = i as f64 / iters as f64;
        let temp = t0 * (t1 / t0).powf(progress);

        if let Some((mut cand, changes)) =
            mutate(&cur, &mut rng, progress, &plan, &groups, rules, budget)
        {
            let gold = evaluate(&mut cand, settings);
            let mut cand_plan = plan.clone();
            let mut cand_groups = groups.clone();
            apply_changes(&mut cand_plan, &mut cand_groups, changes);
            let (score, used) = energy(gold, &cand, &cand_plan, budget, penalty);
            sh.evaluated.fetch_add(1, Ordering::Relaxed);
            let delta = score - cur_score;
            if delta >= 0.0 || rng.random::<f64>() < (delta / temp).exp() {
                cur = cand;
                plan = cand_plan;
                groups = cand_groups;
                cur_score = score;
                if score > best_score + 1e-9 {
                    best_score = score;
                    best = (cur.clone(), plan.clone(), groups.clone());
                    last_improve = i;
                    sh.offer(id, i, gold, score, used, &best.0, &best.1);
                }
            }
        }
        sh.iterations.fetch_add(1, Ordering::Relaxed);

        if i - last_improve > patience {
            cur = best.0.clone();
            plan = best.1.clone();
            groups = best.2.clone();
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
        "Старт: {} потоков x {} итераций, подвижных зданий: {}, top {}, попыток стены: {}",
        threads,
        cfg.max_iterations,
        movable(&layout).len(),
        cfg.top_n,
        cfg.max_walls
    )));
    let groups = initial_groups(&layout, cfg.keep_groups);
    send(OptimizerEvent::Log(format!(
        "Групп: {} (многозвенных: {}), лимит кластера: {}, зазор у складов: {}",
        groups.len(),
        groups.iter().filter(|g| g.members.len() > 1).count(),
        if cfg.keep_groups && cfg.max_cluster_size > 0 {
            cfg.max_cluster_size.to_string()
        } else {
            "нет".to_string()
        },
        if cfg.stockpile_clearance { "вкл" } else { "выкл" }
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