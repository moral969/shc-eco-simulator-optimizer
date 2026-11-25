use std::collections::{HashMap, VecDeque};

use crate::{EntryPoint, buildings::BuildingPlacement, map::CellMap};

use super::{BuildingDistance, DistanceKey, worker_distance::worker_pairs};

pub(crate) fn recompute_building_distances(
    buildings: &[BuildingPlacement],
    map: &CellMap,
) -> HashMap<DistanceKey, BuildingDistance> {
    let mut distances = HashMap::new();

    for start in buildings {
        for finish in buildings {
            if start.id == finish.id {
                continue;
            }

            let key = DistanceKey::new(start.id, finish.id);
            let distance_cells = match (start.entry_point, finish.entry_point) {
                (Some(start_entry), Some(finish_entry)) => {
                    shortest_path_len(map, start_entry, finish_entry)
                }
                _ => None,
            };

            distances.insert(
                key,
                BuildingDistance {
                    key,
                    start_entry: start.entry_point,
                    finish_entry: finish.entry_point,
                    distance_cells,
                },
            );
        }
    }

    distances
}

pub(crate) fn shortest_path_len(
    map: &CellMap,
    start: EntryPoint,
    finish: EntryPoint,
) -> Option<u32> {
    if !map.is_in_bounds(start.x, start.y) || !map.is_in_bounds(finish.x, finish.y) {
        return None;
    }

    if start == finish {
        return Some(0);
    }

    if map.is_blocked(start.x, start.y) || map.is_blocked(finish.x, finish.y) {
        return None;
    }

    let width = map.width();
    let mut dist = vec![u32::MAX; width * map.height()];
    let mut queue = VecDeque::new();

    let start_idx = map.index(start.x, start.y)?;
    dist[start_idx] = 0;
    queue.push_back((start.x, start.y));

    while let Some((x, y)) = queue.pop_front() {
        let current_dist = dist[map.index(x, y)?];

        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                if dx == 0 && dy == 0 {
                    continue;
                }

                let nx = x + dx;
                let ny = y + dy;
                if !map.is_in_bounds(nx, ny) || map.is_blocked(nx, ny) {
                    continue;
                }

                let next_idx = map.index(nx, ny)?;
                if dist[next_idx] != u32::MAX {
                    continue;
                }

                let next_dist = current_dist + 1;
                if nx == finish.x && ny == finish.y {
                    return Some(next_dist);
                }

                dist[next_idx] = next_dist;
                queue.push_back((nx, ny));
            }
        }
    }

    None
}

/// Расстояния от одной точки до всех клеток за один обход (та же восьмисвязная волна, что в shortest_path_len).
fn distances_from(map: &CellMap, start: EntryPoint) -> Option<Vec<u32>> {
    if !map.is_in_bounds(start.x, start.y) || map.is_blocked(start.x, start.y) {
        return None;
    }
    let mut dist = vec![u32::MAX; map.width() * map.height()];
    let mut queue = VecDeque::new();
    dist[map.index(start.x, start.y)?] = 0;
    queue.push_back((start.x, start.y));

    while let Some((x, y)) = queue.pop_front() {
        let current = dist[map.index(x, y)?];
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                if dx == 0 && dy == 0 {
                    continue;
                }
                let (nx, ny) = (x + dx, y + dy);
                if !map.is_in_bounds(nx, ny) || map.is_blocked(nx, ny) {
                    continue;
                }
                let index = map.index(nx, ny)?;
                if dist[index] != u32::MAX {
                    continue;
                }
                dist[index] = current + 1;
                queue.push_back((nx, ny));
            }
        }
    }
    Some(dist)
}

/// Только расстояния, нужные мастерским (как build_worker_distances), одним обходом из каждого
/// старта вместо N^2 обходов. Результат совпадает с recompute_building_distances + build_worker_distances.
pub(crate) fn recompute_worker_distances_fast(
    buildings: &[BuildingPlacement],
    map: &CellMap,
) -> HashMap<DistanceKey, BuildingDistance> {
    let mut by_start: HashMap<u32, Vec<u32>> = HashMap::new();
    for (start, finish) in worker_pairs(buildings) {
        by_start.entry(start).or_default().push(finish);
    }

    let find = |id: u32| buildings.iter().find(|b| b.id == id);
    let mut out = HashMap::new();
    for (start_id, finishes) in by_start {
        let Some(start) = find(start_id) else { continue };
        let dist = start.entry_point.and_then(|entry| distances_from(map, entry));
        for finish_id in finishes {
            let Some(finish) = find(finish_id) else { continue };
            let distance_cells = match (&dist, finish.entry_point) {
                (Some(dist), Some(entry)) => map
                    .index(entry.x, entry.y)
                    .map(|index| dist[index])
                    .filter(|d| *d != u32::MAX),
                _ => None,
            };
            let key = DistanceKey::new(start_id, finish_id);
            out.insert(
                key,
                BuildingDistance {
                    key,
                    start_entry: start.entry_point,
                    finish_entry: finish.entry_point,
                    distance_cells,
                },
            );
        }
    }
    out
}

/// A concrete shortest route for crop approaches; ties prefer the target direction.
/// Native terrain, crowd/substep delays are outside the flat-map planner.
pub(super) fn crop_route(
    map: &CellMap,
    start: EntryPoint,
    target: EntryPoint,
    trim: bool,
) -> Option<(u32, EntryPoint)> {
    if start == target {
        return Some((0, start));
    }
    if !map.is_in_bounds(start.x, start.y)
        || !map.is_in_bounds(target.x, target.y)
        || map.is_blocked(start.x, start.y)
        || map.is_blocked(target.x, target.y)
    {
        return None;
    }
    let mut parents = vec![usize::MAX; map.width() * map.height()];
    let mut queue = VecDeque::from([(start, 0_u32)]);
    let start_index = map.index(start.x, start.y)?;
    parents[start_index] = start_index;
    while let Some((point, distance)) = queue.pop_front() {
        // Y is up in the editor and down in the native crop tables.
        let mut offsets = [
            (-1, 1),
            (0, 1),
            (1, 1),
            (-1, 0),
            (1, 0),
            (-1, -1),
            (0, -1),
            (1, -1),
        ];
        offsets.sort_by_key(|(dx, dy)| {
            let x = i64::from(point.x + dx) - i64::from(target.x);
            let y = i64::from(point.y + dy) - i64::from(target.y);
            x * x + y * y
        });
        for (dx, dy) in offsets {
            let next = EntryPoint {
                x: point.x + dx,
                y: point.y + dy,
            };
            let Some(index) = map.index(next.x, next.y) else {
                continue;
            };
            if map.is_blocked(next.x, next.y) || parents[index] != usize::MAX {
                continue;
            }
            parents[index] = map.index(point.x, point.y)?;
            if next == target {
                return if trim {
                    Some((distance, point))
                } else {
                    Some((distance + 1, target))
                };
            }
            queue.push_back((next, distance + 1));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::{CellMap, EntryPoint};

    use super::shortest_path_len;

    #[test]
    fn same_start_and_finish_cell_has_zero_distance() {
        let map = CellMap::new(10);
        let point = EntryPoint { x: 1, y: 1 };
        assert_eq!(shortest_path_len(&map, point, point), Some(0));
    }
}
