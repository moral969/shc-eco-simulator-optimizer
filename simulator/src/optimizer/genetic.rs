//! Генетический алгоритм: остров на поток. Геном — план построения (PlanStep: здание, позиция,
//! стена ряда), поэтому трюк со стеной переживает скрещивание и мутации. Операторы мутации общие
//! с отжигом (mutate: relocate / wall_row / snap / swap), оценка — evaluate + judge.

use std::{
    cmp::Ordering as Cmp,
    collections::HashMap,
    sync::{Arc, atomic::Ordering},
};

use rand::Rng;

use crate::{BuildingType, Simulator};

use super::{
    Eval, PlanStep, Shared, WallBudget, WallPlan, apply_changes, evaluate, judge, movable, mutate,
    normalize_plan, pick_position,
};

/// Сдвиг id групп второго родителя, чтобы не слиться с группами первого (decode перенумерует).
const GROUP_SHIFT: u32 = 1_000_000;

#[derive(Clone)]
struct Individual {
    sim: Simulator,
    plan: Vec<PlanStep>,
    eval: Eval,
}

fn by_score(a: &Individual, b: &Individual) -> Cmp {
    a.eval
        .score
        .partial_cmp(&b.eval.score)
        .unwrap_or(Cmp::Equal)
}

/// Оценка особи; каждая оценка идёт в общий счётчик прогресса.
fn assess(mut sim: Simulator, plan: Vec<PlanStep>, sh: &Shared, budget: WallBudget) -> Individual {
    let gold = evaluate(&mut sim, sh.settings);
    let eval = judge(gold, &sim, &plan, budget, &sh.cfg);
    sh.evaluated.fetch_add(1, Ordering::Relaxed);
    sh.iterations.fetch_add(1, Ordering::Relaxed);
    // Особь живёт долго: освобождаем таблицы расстояний.
    sim.set_defer_path_calculation(true);
    Individual { sim, plan, eval }
}

/// Неподвижная часть раскладки: исходная без подвижных зданий.
fn empty_base(layout: &Simulator) -> Simulator {
    let mut base = layout.clone();
    for (id, ..) in movable(layout) {
        base.remove_building(id);
    }
    base.set_defer_path_calculation(true);
    base
}

/// Турнир: лучший из k случайных особей.
fn pick<'a>(population: &'a [Individual], k: usize, rng: &mut impl Rng) -> &'a Individual {
    (0..k.max(1))
        .map(|_| &population[rng.random_range(0..population.len())])
        .max_by(|a, b| by_score(a, b))
        .expect("population is not empty")
}

/// Поставить здание точно в (x, y), иначе в ближайшее свободное место.
fn place_near(
    sim: &mut Simulator,
    rng: &mut impl Rng,
    t: BuildingType,
    x: i32,
    y: i32,
) -> Option<(i32, i32)> {
    if sim.place_building(t, x, y).is_ok() {
        return Some((x, y));
    }
    for radius in 1..=12 {
        for _ in 0..6 {
            let (nx, ny) = pick_position(sim, rng, t, (x, y), radius)?;
            if sim.place_building(t, nx, ny).is_ok() {
                return Some((nx, ny));
            }
        }
    }
    None
}

/// План -> расстановка: на базу (неподвижная часть) ставим здания по плану. У ряда под стеной
/// сначала стена, потом здания ряда, потом стена снимается (если не kept). Если место занято,
/// здание ставится рядом и теряет стену. Группы перенумеровываются.
fn decode(
    base: &Simulator,
    plan: &[PlanStep],
    rng: &mut impl Rng,
) -> Option<(Simulator, Vec<PlanStep>)> {
    let plan = normalize_plan(plan);
    let mut sim = base.clone();
    let mut out: Vec<PlanStep> = Vec::with_capacity(plan.len());
    let mut groups: HashMap<u32, u32> = HashMap::new();

    for (i, step) in plan.iter().enumerate() {
        let Some(wall) = &step.wall else {
            let (x, y) = place_near(&mut sim, rng, step.building_type, step.x, step.y)?;
            out.push(PlanStep {
                building_type: step.building_type,
                x,
                y,
                wall: None,
            });
            continue;
        };
        if groups.contains_key(&wall.group) {
            continue;
        }
        let new_id = groups.len() as u32 + 1;
        groups.insert(wall.group, new_id);

        let wall_ok = sim.place_wall(wall.sx, wall.sy, wall.ex, wall.ey).is_ok();
        for member in plan[i..]
            .iter()
            .filter(|s| s.wall.as_ref().is_some_and(|w| w.group == wall.group))
        {
            let (x, y) = place_near(&mut sim, rng, member.building_type, member.x, member.y)?;
            let keep = wall_ok && (x, y) == (member.x, member.y);
            out.push(PlanStep {
                building_type: member.building_type,
                x,
                y,
                wall: keep.then(|| WallPlan {
                    group: new_id,
                    ..wall.clone()
                }),
            });
        }
        if wall_ok && !wall.kept {
            // клетки стены были свободны, поэтому последняя стена в этой клетке — наша
            sim.remove_whole_wall_at(wall.sx, wall.sy);
        }
    }
    Some((sim, out))
}

/// Скрещивание: для каждого типа здания берём все его шаги у одного из родителей (счётчики типов
/// сохраняются). Ряды под стеной переезжают вместе с шагами; ряды сверх лимита стен теряют стену
/// (здания остаются на местах).
fn crossover(
    base: &Simulator,
    a: &[PlanStep],
    b: &[PlanStep],
    rng: &mut impl Rng,
    budget: WallBudget,
) -> Option<(Simulator, Vec<PlanStep>)> {
    let mut types: Vec<BuildingType> = Vec::new();
    for step in a {
        if !types.contains(&step.building_type) {
            types.push(step.building_type);
        }
    }

    let mut plan: Vec<PlanStep> = Vec::with_capacity(a.len());
    for t in types {
        let from_b = rng.random_bool(0.5);
        let parent = if from_b { b } else { a };
        for step in parent.iter().filter(|s| s.building_type == t) {
            let mut step = step.clone();
            if from_b {
                if let Some(w) = step.wall.as_mut() {
                    w.group += GROUP_SHIFT;
                }
            }
            plan.push(step);
        }
    }

    // Лимит стен: оставляем первые budget.max групп.
    let mut seen: Vec<u32> = Vec::new();
    for step in &plan {
        if let Some(w) = &step.wall {
            if !seen.contains(&w.group) {
                seen.push(w.group);
            }
        }
    }
    seen.truncate(budget.max);
    for step in &mut plan {
        if step.wall.as_ref().is_some_and(|w| !seen.contains(&w.group)) {
            step.wall = None;
        }
    }
    decode(base, &plan, rng)
}

pub(super) fn run_genetic(id: usize, start: Simulator, sh: Arc<Shared>) {
    let mut rng = rand::rng();
    let cfg = &sh.cfg;
    let iters = cfg.max_iterations.max(1);
    let size = cfg.population.max(4);
    let elite = cfg.elite.clamp(1, size - 1);
    let budget = WallBudget {
        max: cfg.max_walls,
        base: start.walls().len(),
    };
    let style_on = cfg.style.is_active();

    let mut seed = start;
    seed.set_defer_path_calculation(true);
    seed.disable_build_log();
    let seed_plan: Vec<PlanStep> = movable(&seed)
        .into_iter()
        .map(|(_, t, x, y)| PlanStep {
            building_type: t,
            x,
            y,
            wall: None,
        })
        .collect();
    let base = empty_base(&seed);

    // Начальная популяция: исходная раскладка + «перемешанные» копии.
    let mut population: Vec<Individual> = Vec::with_capacity(size);
    population.push(assess(seed.clone(), seed_plan.clone(), &sh, budget));
    while population.len() < size {
        if sh.cancel.load(Ordering::Relaxed) {
            return;
        }
        let (mut s, mut p) = (seed.clone(), seed_plan.clone());
        for _ in 0..p.len() * 3 {
            if let Some((next, ch)) = mutate(&s, &mut rng, 0.0, &p, budget, style_on) {
                s = next;
                apply_changes(&mut p, ch);
            }
        }
        population.push(assess(s, p, &sh, budget));
    }

    let mut done = population.len();
    let mut best_score = f64::NEG_INFINITY;
    while done < iters && !sh.cancel.load(Ordering::Relaxed) {
        population.sort_by(|a, b| by_score(b, a));
        if population[0].eval.score > best_score + 1e-9 {
            best_score = population[0].eval.score;
            let top = &population[0];
            sh.offer(id, done, top.eval, &top.sim, &top.plan);
        }

        let progress = done as f64 / iters as f64;
        let mut next: Vec<Individual> = population.iter().take(elite).cloned().collect();
        while next.len() < size && done < iters {
            let a = pick(&population, cfg.tournament, &mut rng);
            let b = pick(&population, cfg.tournament, &mut rng);
            let crossed = if rng.random_bool(cfg.crossover_rate.clamp(0.0, 1.0)) {
                crossover(&base, &a.plan, &b.plan, &mut rng, budget)
            } else {
                None
            };
            let (mut sim, mut plan) = crossed.unwrap_or_else(|| (a.sim.clone(), a.plan.clone()));

            if rng.random_bool(cfg.mutation_rate.clamp(0.0, 1.0)) {
                for _ in 0..rng.random_range(1..=2) {
                    if let Some((s, ch)) = mutate(&sim, &mut rng, progress, &plan, budget, style_on)
                    {
                        sim = s;
                        apply_changes(&mut plan, ch);
                    }
                }
            }
            next.push(assess(sim, plan, &sh, budget));
            done += 1;
        }
        population = next;
    }

    population.sort_by(|a, b| by_score(b, a));
    let top = &population[0];
    sh.offer(id, done, top.eval, &top.sim, &top.plan);
}