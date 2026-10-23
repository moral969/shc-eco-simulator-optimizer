use std::collections::VecDeque;

use crate::{
    buildings::{BuildingType, EntryPoint},
    map::CellMap,
    walls::WallSegment,
};

pub(crate) fn calculate_building_entry(
    map: &CellMap,
    walls: &[WallSegment],
    building_type: BuildingType,
    x: i32,
    y: i32,
    size: usize,
) -> Option<EntryPoint> {
    if building_type == BuildingType::GoodsYard {
        return None;
    }

    if building_type == BuildingType::WheatFarm {
        let entry = EntryPoint { x: x + 4, y: y + 3 };
        return entry_is_accessible(map, entry).then_some(entry);
    }

    let rotation_steps = if is_workshop(building_type) {
        workshop_wall_rotation_steps(walls, x, y, size)
    } else {
        0
    };

    resolve_entry_point_for_square(map, x, y, size, rotation_steps)
}

pub(crate) fn resolve_entry_point_for_square(
    map: &CellMap,
    x: i32,
    y: i32,
    size: usize,
    rotation_steps: usize,
) -> Option<EntryPoint> {
    let default_cell = default_entry_cell_rotated(x, y, size, rotation_steps);
    let side_cells = side_perimeter_cells_clockwise(x, y, size, default_cell);
    if let Some(point) = first_available(map, side_cells) {
        return Some(point);
    }

    let corner_cells = corner_cells_clockwise_from_bottom_right(x, y, size, rotation_steps);
    first_available(map, corner_cells)
}

fn first_available(map: &CellMap, cells: Vec<(i32, i32)>) -> Option<EntryPoint> {
    for (cx, cy) in cells {
        let entry = EntryPoint { x: cx, y: cy };
        if entry_is_accessible(map, entry) {
            return Some(entry);
        }
    }

    None
}

pub(crate) fn entry_is_accessible(map: &CellMap, entry: EntryPoint) -> bool {
    if !map.is_in_bounds(entry.x, entry.y) || map.is_blocked(entry.x, entry.y) {
        return false;
    }

    // A bounded eight-way search rejects sealed pockets without calculating economy routes.
    const RADIUS: i32 = 6;
    const WIDTH: usize = (RADIUS * 2 + 1) as usize;
    let mut visited = [false; WIDTH * WIDTH];
    let mut queue = VecDeque::new();
    visited[RADIUS as usize * WIDTH + RADIUS as usize] = true;
    queue.push_back((0i32, 0i32));
    let bounds = map.bounds();

    while let Some((dx, dy)) = queue.pop_front() {
        let x = entry.x + dx;
        let y = entry.y + dy;
        if dx.abs() == RADIUS
            || dy.abs() == RADIUS
            || x == bounds.min_x
            || y == bounds.min_y
            || x == bounds.max_x - 1
            || y == bounds.max_y - 1
        {
            return true;
        }
        for step_y in -1..=1 {
            for step_x in -1..=1 {
                if step_x == 0 && step_y == 0 {
                    continue;
                }
                let nx = dx + step_x;
                let ny = dy + step_y;
                let index = (ny + RADIUS) as usize * WIDTH + (nx + RADIUS) as usize;
                if visited[index] {
                    continue;
                }
                visited[index] = true;
                let (Some(x), Some(y)) = (entry.x.checked_add(nx), entry.y.checked_add(ny)) else {
                    continue;
                };
                if map.is_in_bounds(x, y) && !map.is_blocked(x, y) {
                    queue.push_back((nx, ny));
                }
            }
        }
    }
    false
}

fn workshop_wall_rotation_steps(walls: &[WallSegment], x: i32, y: i32, size: usize) -> usize {
    let xi = x;
    let yi = y;
    let ni = size as i32;

    if side_has_wall_contact(walls, (xi..(xi + ni)).map(|cx| (cx, yi + ni)).collect()) {
        return 0;
    }
    if side_has_wall_contact(walls, (yi..(yi + ni)).map(|cy| (xi + ni, cy)).collect()) {
        return 1;
    }
    if side_has_wall_contact(walls, (xi..(xi + ni)).map(|cx| (cx, yi - 1)).collect()) {
        return 2;
    }
    if side_has_wall_contact(walls, (yi..(yi + ni)).map(|cy| (xi - 1, cy)).collect()) {
        return 3;
    }

    0
}

fn side_has_wall_contact(walls: &[WallSegment], cells: Vec<(i32, i32)>) -> bool {
    !cells.is_empty() && cells.into_iter().all(|(x, y)| is_wall_at(walls, x, y))
}

fn is_wall_at(walls: &[WallSegment], x: i32, y: i32) -> bool {
    walls.iter().any(|wall| wall_contains_cell(wall, x, y))
}

fn default_entry_cell_rotated(
    x: i32,
    y: i32,
    size: usize,
    rotation_steps: usize,
) -> Option<(i32, i32)> {
    if size == 0 {
        return None;
    }

    let offset = if size == 2 { 0 } else { (size / 2) as i32 };
    let xi = x;
    let yi = y;
    let ni = size as i32;
    let mirrored_offset = ni - 1 - offset;

    match rotation_steps % 4 {
        0 => Some((xi + offset, yi - 1)),
        1 => Some((xi - 1, yi + mirrored_offset)),
        2 => Some((xi + mirrored_offset, yi + ni)),
        _ => Some((xi + ni, yi + offset)),
    }
}

fn side_perimeter_cells_clockwise(
    x: i32,
    y: i32,
    size: usize,
    start: Option<(i32, i32)>,
) -> Vec<(i32, i32)> {
    if size == 0 {
        return Vec::new();
    }

    let xi = x;
    let yi = y;
    let ni = size as i32;
    let top = yi + ni;
    let right = xi + ni;

    let mut ring = Vec::with_capacity(size * 4);

    for cx in (xi..(xi + ni)).rev() {
        ring.push((cx, yi - 1));
    }
    for cy in yi..(yi + ni) {
        ring.push((xi - 1, cy));
    }
    for cx in xi..(xi + ni) {
        ring.push((cx, top));
    }
    for cy in (yi..(yi + ni)).rev() {
        ring.push((right, cy));
    }

    if let Some(start_cell) = start
        && let Some(idx) = ring.iter().position(|cell| *cell == start_cell)
    {
        ring.rotate_left(idx);
    }

    ring
}

fn corner_cells_clockwise_from_bottom_right(
    x: i32,
    y: i32,
    size: usize,
    rotation_steps: usize,
) -> Vec<(i32, i32)> {
    let xi = x;
    let yi = y;
    let ni = size as i32;

    let mut corners = vec![
        (xi + ni, yi - 1),
        (xi - 1, yi - 1),
        (xi - 1, yi + ni),
        (xi + ni, yi + ni),
    ];

    corners.rotate_left(rotation_steps % 4);
    corners
}

pub(crate) fn wall_contains_cell(wall: &WallSegment, x: i32, y: i32) -> bool {
    if wall.start_x == wall.end_x {
        if x != wall.start_x {
            return false;
        }

        let min_y = wall.start_y.min(wall.end_y);
        let max_y = wall.start_y.max(wall.end_y);
        return y >= min_y && y <= max_y;
    }

    if wall.start_y == wall.end_y {
        if y != wall.start_y {
            return false;
        }

        let min_x = wall.start_x.min(wall.end_x);
        let max_x = wall.start_x.max(wall.end_x);
        return x >= min_x && x <= max_x;
    }

    false
}

fn is_workshop(building_type: BuildingType) -> bool {
    matches!(
        building_type,
        BuildingType::FletchersWorkshop
            | BuildingType::BlacksmithsWorkshop
            | BuildingType::PoleturnersWorkshop
            | BuildingType::ArmourersWorkshop
            | BuildingType::Bakery
    )
}
