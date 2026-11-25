//! Оценка «аккуратности» расстановки: выравнивание, ряды вплотную, диагонали, симметрия, компактность.
//! Ядро не трогает: смотрит только на координаты мастерских и оружеек (блоки 4x4).

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{BuildingType, Simulator};

/// Сторона блока (мастерская / оружейка).
const N: i32 = 4;

/// Веса метрик в gold/мин: бонус = вес * метрика (0..1). 0 — метрика не учитывается.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StyleWeights {
    /// Здания на общей линии по X или Y.
    pub align: f64,
    /// Здания вплотную цепочками в линию.
    pub rows: f64,
    /// Партнёр по диагонали на решётке 4 клетки.
    pub diagonal: f64,
    /// Зеркальная (по X, по Y или центральная) расстановка.
    pub symmetry: f64,
    /// Доля площади зданий в габарите всей группы.
    pub compact: f64,
    /// Длина ряда, после которой бонус за ряд перестаёт расти (2..8).
    pub row_target: usize,
}

impl Default for StyleWeights {
    fn default() -> Self {
        Self {
            align: 10.0,
            rows: 20.0,
            diagonal: 0.0,
            symmetry: 15.0,
            compact: 0.0,
            row_target: 4,
        }
    }
}

impl StyleWeights {
    /// Хотя бы один вес включён.
    pub fn is_active(&self) -> bool {
        self.align > 0.0
            || self.rows > 0.0
            || self.diagonal > 0.0
            || self.symmetry > 0.0
            || self.compact > 0.0
    }
}

/// Значения метрик, каждое 0..1.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct StyleMetrics {
    pub align: f64,
    pub rows: f64,
    pub diagonal: f64,
    pub symmetry: f64,
    pub compact: f64,
}

impl StyleMetrics {
    /// Бонус к оценке в gold/мин.
    pub fn bonus(&self, weights: &StyleWeights) -> f64 {
        weights.align * self.align
            + weights.rows * self.rows
            + weights.diagonal * self.diagonal
            + weights.symmetry * self.symmetry
            + weights.compact * self.compact
    }
}

fn is_block(t: BuildingType) -> bool {
    matches!(
        t,
        BuildingType::Armoury
            | BuildingType::FletchersWorkshop
            | BuildingType::BlacksmithsWorkshop
            | BuildingType::PoleturnersWorkshop
            | BuildingType::ArmourersWorkshop
    )
}

pub fn style_metrics(sim: &Simulator, row_target: usize) -> StyleMetrics {
    let cells: Vec<(i32, i32)> = sim
        .buildings()
        .iter()
        .filter(|b| is_block(b.building_type))
        .map(|b| (b.x, b.y))
        .collect();
    let n = cells.len();
    if n < 2 {
        return StyleMetrics::default();
    }
    let total = n as f64;
    let set: HashSet<(i32, i32)> = cells.iter().copied().collect();

    // Выравнивание: здание делит X или Y хотя бы с одним другим (по половине за каждую ось).
    let mut columns: HashMap<i32, usize> = HashMap::new();
    let mut lines: HashMap<i32, usize> = HashMap::new();
    for &(x, y) in &cells {
        *columns.entry(x).or_default() += 1;
        *lines.entry(y).or_default() += 1;
    }
    let align = cells
        .iter()
        .map(|&(x, y)| {
            (usize::from(columns[&x] > 1) + usize::from(lines[&y] > 1)) as f64 / 2.0
        })
        .sum::<f64>()
        / total;

    // Ряды: цепочки вплотную по X или по Y. Цепочка длины len даёт каждому зданию
    // min(1, (len - 1) / (target - 1)); здание в двух цепочках берёт лучшую.
    let target = row_target.max(2) as f64;
    let mut credit: HashMap<(i32, i32), f64> = HashMap::new();
    for &(x, y) in &cells {
        for (dx, dy) in [(N, 0), (0, N)] {
            if set.contains(&(x - dx, y - dy)) {
                continue; // не начало цепочки
            }
            let mut len = 1;
            while set.contains(&(x + dx * len, y + dy * len)) {
                len += 1;
            }
            if len < 2 {
                continue;
            }
            let value = ((len - 1) as f64 / (target - 1.0)).min(1.0);
            for k in 0..len {
                let entry = credit.entry((x + dx * k, y + dy * k)).or_insert(0.0);
                *entry = entry.max(value);
            }
        }
    }
    let rows = credit.values().sum::<f64>() / total;

    // Диагонали: партнёр на той же решётке 4 клетки по диагонали.
    let diagonal = cells
        .iter()
        .filter(|&&(x, y)| {
            cells.iter().any(|&(u, v)| {
                u != x && (u - x).abs() == (v - y).abs() && (u - x).abs() % N == 0
            })
        })
        .count() as f64
        / total;

    // Симметрия: ось через центр габарита. Для блоков 4x4 зеркало левого края x — это sum_x - x.
    let min_x = cells.iter().map(|c| c.0).min().unwrap_or(0);
    let max_x = cells.iter().map(|c| c.0).max().unwrap_or(0);
    let min_y = cells.iter().map(|c| c.1).min().unwrap_or(0);
    let max_y = cells.iter().map(|c| c.1).max().unwrap_or(0);
    let (sum_x, sum_y) = (min_x + max_x, min_y + max_y);
    let matched = |mirror: &dyn Fn((i32, i32)) -> (i32, i32)| {
        cells.iter().filter(|&&c| set.contains(&mirror(c))).count() as f64 / total
    };
    let symmetry = matched(&|(x, y)| (sum_x - x, y))
        .max(matched(&|(x, y)| (x, sum_y - y)))
        .max(matched(&|(x, y)| (sum_x - x, sum_y - y)));

    // Компактность: площадь зданий / площадь габарита.
    let area = (max_x - min_x + N) * (max_y - min_y + N);
    let compact = (n as i32 * N * N) as f64 / area as f64;

    StyleMetrics {
        align,
        rows,
        diagonal,
        symmetry,
        compact: compact.min(1.0),
    }
}

#[cfg(test)]
mod tests {
    use crate::{BuildingType, Simulator};

    use super::style_metrics;

    #[test]
    fn tight_symmetric_row_scores_higher_than_scattered_blocks() {
        let mut row = Simulator::new(40).expect("map");
        for x in [10, 14, 18, 22] {
            row.place_building(BuildingType::FletchersWorkshop, x, 10)
                .expect("block");
        }
        let mut scattered = Simulator::new(40).expect("map");
        for (x, y) in [(10, 10), (17, 13), (25, 11), (31, 19)] {
            scattered
                .place_building(BuildingType::FletchersWorkshop, x, y)
                .expect("block");
        }

        let a = style_metrics(&row, 4);
        let b = style_metrics(&scattered, 4);
        assert!((a.rows - 1.0).abs() < 1e-9);
        assert_eq!(b.rows, 0.0);
        assert!(a.symmetry > b.symmetry);
        assert!(a.align > b.align);
    }
}