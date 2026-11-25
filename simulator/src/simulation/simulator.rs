use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::{
    SavedBuilding,
    build_log::BuildStep,
    buildings::{BuildingFactory, BuildingPlacement, BuildingType, StockpileResource},
    map::{CellMap, MapBounds, MapError},
    project::validate_unique_ids,
    walls::{WallSegment, line_cells},
};

use super::{
    BreadEconomyReport, BuildingDistance, DistanceKey, GameTime,
    bread::calculate_bread_economy,
    entry_logic::{
        calculate_building_entry, entry_is_accessible, resolve_entry_point_for_square,
        wall_contains_cell,
    },
    pathfinding::recompute_building_distances,
    production_cycle::{
        ProductionCycle, ProductionCycleError, ProductionRouteUsage, SimulationSettings,
        WeaponType, distance_cells, find_building, find_stockpile_for_resource,
        travel_ticks_for_distance,
    },
    worker_distance::build_worker_distances,
};

#[derive(Debug, Clone)]
pub struct Simulator {
    map: CellMap,
    factory: BuildingFactory,
    time: GameTime,
    buildings: Vec<BuildingPlacement>,
    walls: Vec<WallSegment>,
    next_wall_id: u32,
    distances: HashMap<DistanceKey, BuildingDistance>,
    worker_distances: HashMap<DistanceKey, BuildingDistance>,
    defer_path_calculation: bool,
    next_wheat_layout: u8,
    bread_capacities: RefCell<Option<(i32, super::bread::BreadCapacities)>>,
    /// Последовательность действий, которыми построена текущая расстановка.
    build_log: Vec<BuildStep>,
    /// false — журнал не пишется (оптимизатор или загруженный проект без последовательности).
    record_build_log: bool,
}

#[derive(Debug)]
pub enum SimulatorError {
    Map(MapError),
    InvalidMapSize,
    InvalidWallDirection,
    StockpileDesignationRequiresStockpile,
    InvalidProject(&'static str),
    UnsupportedProjectVersion { version: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveOutcome {
    None,
    Buildings {
        removed_ids: Vec<u32>,
        goods_yard_group_id: Option<u32>,
    },
    Wall {
        id: u32,
    },
    WallCell {
        affected_wall_ids: Vec<u32>,
    },
}

impl core::fmt::Display for SimulatorError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Map(error) => write!(f, "{error}"),
            Self::InvalidMapSize => write!(f, "map size must be greater than zero"),
            Self::InvalidWallDirection => write!(f, "wall must be horizontal or vertical"),
            Self::StockpileDesignationRequiresStockpile => {
                write!(
                    f,
                    "stockpile designation can only be applied to a stockpile"
                )
            }
            Self::InvalidProject(message) => write!(f, "invalid project: {message}"),
            Self::UnsupportedProjectVersion { version } => {
                write!(f, "project format version {version} is not supported")
            }
        }
    }
}

impl std::error::Error for SimulatorError {}

impl From<MapError> for SimulatorError {
    fn from(value: MapError) -> Self {
        Self::Map(value)
    }
}

fn is_entry_point_available(
    map: &CellMap,
    entry_point: Option<crate::buildings::EntryPoint>,
) -> bool {
    let Some(entry_point) = entry_point else {
        return false;
    };

    entry_is_accessible(map, entry_point)
}

fn wall_pieces_without_cell(wall: WallSegment, x: i32, y: i32) -> Vec<WallSegment> {
    let mut pieces = Vec::with_capacity(2);

    if wall.start_x == wall.end_x {
        let min_y = wall.start_y.min(wall.end_y);
        let max_y = wall.start_y.max(wall.end_y);
        if y > min_y {
            pieces.push(WallSegment::new(wall.id, x, min_y, x, y - 1));
        }
        if y < max_y {
            pieces.push(WallSegment::new(wall.id, x, y + 1, x, max_y));
        }
        return pieces;
    }

    let min_x = wall.start_x.min(wall.end_x);
    let max_x = wall.start_x.max(wall.end_x);
    if x > min_x {
        pieces.push(WallSegment::new(wall.id, min_x, y, x - 1, y));
    }
    if x < max_x {
        pieces.push(WallSegment::new(wall.id, x + 1, y, max_x, y));
    }
    pieces
}

impl Simulator {
    pub fn new(map_size: usize) -> Result<Self, SimulatorError> {
        let bounds = MapBounds::square(map_size).ok_or(SimulatorError::InvalidMapSize)?;
        Self::with_bounds(bounds)
    }

    pub fn with_bounds(bounds: MapBounds) -> Result<Self, SimulatorError> {
        let map = CellMap::with_bounds(bounds).ok_or(SimulatorError::InvalidMapSize)?;

        Ok(Self {
            map,
            factory: BuildingFactory::new(),
            time: GameTime::new(),
            buildings: Vec::new(),
            walls: Vec::new(),
            next_wall_id: 1,
            distances: HashMap::new(),
            worker_distances: HashMap::new(),
            defer_path_calculation: false,
            next_wheat_layout: 0,
            bread_capacities: RefCell::new(None),
            build_log: Vec::new(),
            record_build_log: true,
        })
    }

    pub fn from_saved_layout(
        bounds: MapBounds,
        mut saved_buildings: Vec<SavedBuilding>,
        walls: Vec<WallSegment>,
    ) -> Result<Self, SimulatorError> {
        if !bounds.is_valid() {
            return Err(SimulatorError::InvalidMapSize);
        }
        validate_unique_ids(&saved_buildings, &walls)?;
        let mut next_wheat_layout = 0;
        for building in &mut saved_buildings {
            if building.building_type == BuildingType::WheatFarm {
                let layout = building.wheat_layout.unwrap_or(next_wheat_layout);
                if layout > 1 {
                    return Err(SimulatorError::InvalidProject(
                        "wheat layout must be 0 or 1",
                    ));
                }
                building.wheat_layout = Some(layout);
                next_wheat_layout = 1 - layout;
            }
        }

        let next_building_id = saved_buildings
            .iter()
            .map(|building| building.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(SimulatorError::InvalidProject("building ID is too large"))?;
        let next_group_id = saved_buildings
            .iter()
            .filter_map(|building| building.goods_yard_group_id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(SimulatorError::InvalidProject(
                "goods yard group ID is too large",
            ))?;
        let next_wall_id = walls
            .iter()
            .map(|wall| wall.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(SimulatorError::InvalidProject("wall ID is too large"))?;

        let mut map = CellMap::with_bounds(bounds).ok_or(SimulatorError::InvalidMapSize)?;
        let buildings = saved_buildings
            .into_iter()
            .map(SavedBuilding::into_placement)
            .collect::<Vec<_>>();
        for building in &buildings {
            if let Some(entry) = building.entry_point
                && !map.is_in_bounds(entry.x, entry.y)
            {
                return Err(SimulatorError::InvalidProject(
                    "building entry point is outside the map",
                ));
            }
            map.place(building)?;
        }
        for wall in &walls {
            if !wall.is_axis_aligned() {
                return Err(SimulatorError::InvalidWallDirection);
            }
            map.place_wall_cells(wall.cells())?;
        }

        // Для непустой раскладки без известной последовательности журнал вести нельзя:
        // он не был бы полным. Его включит set_build_log или remove_all.
        let empty_layout = buildings.is_empty() && walls.is_empty();
        let mut simulator = Self {
            map,
            factory: BuildingFactory::with_next_ids(next_building_id, next_group_id),
            time: GameTime::new(),
            buildings,
            walls,
            next_wall_id,
            next_wheat_layout,
            bread_capacities: RefCell::new(None),
            distances: HashMap::new(),
            worker_distances: HashMap::new(),
            defer_path_calculation: false,
            build_log: Vec::new(),
            record_build_log: empty_layout,
        };
        simulator.refresh_unavailable_entry_points();
        simulator.recompute_distances();
        Ok(simulator)
    }

    pub fn map_size(&self) -> usize {
        self.map.width()
    }

    pub(crate) fn next_wheat_layout(&self) -> u8 {
        self.next_wheat_layout
    }

    pub(crate) fn restore_next_wheat_layout(&mut self, layout: u8) -> Result<(), SimulatorError> {
        if layout > 1 {
            return Err(SimulatorError::InvalidProject(
                "next wheat layout must be 0 or 1",
            ));
        }
        self.next_wheat_layout = layout;
        Ok(())
    }

    pub fn map_bounds(&self) -> MapBounds {
        self.map.bounds()
    }

    pub fn map_width(&self) -> usize {
        self.map.width()
    }

    pub fn map_height(&self) -> usize {
        self.map.height()
    }

    pub fn time(&self) -> GameTime {
        self.time
    }

    pub fn buildings(&self) -> &[BuildingPlacement] {
        &self.buildings
    }

    pub fn walls(&self) -> &[WallSegment] {
        &self.walls
    }

    pub fn distances(&self) -> &HashMap<DistanceKey, BuildingDistance> {
        &self.distances
    }

    pub fn worker_distances(&self) -> &HashMap<DistanceKey, BuildingDistance> {
        &self.worker_distances
    }

    pub fn distance_between(
        &self,
        start_building_id: u32,
        finish_building_id: u32,
    ) -> Option<&BuildingDistance> {
        self.distances
            .get(&DistanceKey::new(start_building_id, finish_building_id))
    }

    pub fn worker_distance_between(
        &self,
        start_building_id: u32,
        finish_building_id: u32,
    ) -> Option<&BuildingDistance> {
        self.worker_distances
            .get(&DistanceKey::new(start_building_id, finish_building_id))
    }

    pub fn is_cell_occupied(&self, x: i32, y: i32) -> bool {
        self.map.is_occupied(x, y)
    }

    pub fn place_building(
        &mut self,
        building_type: BuildingType,
        x: i32,
        y: i32,
    ) -> Result<u32, SimulatorError> {
        let footprint = crate::Footprint::for_type(building_type);
        let mut candidate = self.clone();
        candidate.ensure_bounds_for_extent(
            x,
            y,
            x + footprint.width() as i32,
            y + footprint.height() as i32,
        )?;
        let id = candidate.place_building_in_bounds(building_type, x, y)?;
        *self = candidate;
        self.record_step(BuildStep::Building { building_type, x, y });
        Ok(id)
    }

    fn place_building_in_bounds(
        &mut self,
        building_type: BuildingType,
        x: i32,
        y: i32,
    ) -> Result<u32, SimulatorError> {
        if building_type == BuildingType::GoodsYard {
            let (_, mut stacks) = self.factory.create_goods_yard_stacks(x, y);
            for stack in &stacks {
                self.map.can_place(stack)?;
            }

            let first_id = stacks[0].id;
            for stack in &mut stacks {
                self.map.place(stack)?;
                self.assign_entry_points(stack);
            }

            self.buildings.extend(stacks);
            self.refresh_unavailable_entry_points();
            self.recompute_distances();
            return Ok(first_id);
        }

        let mut placement = self.factory.create(building_type, x, y);
        if building_type == BuildingType::WheatFarm {
            placement.wheat_layout = self.next_wheat_layout;
            self.next_wheat_layout ^= 1;
        }
        self.map.place(&placement)?;
        self.assign_entry_points(&mut placement);
        let id = placement.id;
        self.buildings.push(placement);
        self.refresh_unavailable_entry_points();
        self.recompute_distances();
        Ok(id)
    }

    pub fn place_wall(
        &mut self,
        start_x: i32,
        start_y: i32,
        end_x: i32,
        end_y: i32,
    ) -> Result<u32, SimulatorError> {
        let mut candidate = self.clone();
        candidate.ensure_bounds_for_extent(
            start_x.min(end_x),
            start_y.min(end_y),
            start_x.max(end_x) + 1,
            start_y.max(end_y) + 1,
        )?;
        let id = candidate.place_wall_in_bounds(start_x, start_y, end_x, end_y)?;
        *self = candidate;
        self.record_step(BuildStep::Wall {
            start_x,
            start_y,
            end_x,
            end_y,
        });
        Ok(id)
    }

    fn place_wall_in_bounds(
        &mut self,
        start_x: i32,
        start_y: i32,
        end_x: i32,
        end_y: i32,
    ) -> Result<u32, SimulatorError> {
        let wall = WallSegment::new(self.next_wall_id, start_x, start_y, end_x, end_y);
        if !wall.is_axis_aligned() {
            return Err(SimulatorError::InvalidWallDirection);
        }

        let cells = line_cells(start_x, start_y, end_x, end_y);
        self.map.place_wall_cells(cells.iter().copied())?;
        self.walls.push(wall);
        self.next_wall_id += 1;
        self.refresh_unavailable_entry_points();
        self.recompute_distances();

        Ok(wall.id)
    }

    pub fn remove_building(&mut self, building_id: u32) -> bool {
        let Some((goods_yard_group_id, x, y)) = self
            .buildings
            .iter()
            .find(|building| building.id == building_id)
            .map(|building| (building.goods_yard_group_id, building.x, building.y))
        else {
            return false;
        };

        let removed = if let Some(group_id) = goods_yard_group_id {
            self.remove_buildings_by_group(group_id)
        } else {
            self.remove_buildings_by_ids([building_id])
        };

        if removed.is_empty() {
            return false;
        }
        self.record_step(BuildStep::Remove {
            x,
            y,
            whole_wall: false,
        });
        true
    }

    pub fn remove_at(&mut self, x: i32, y: i32) -> RemoveOutcome {
        let outcome = self.remove_at_with_wall_behavior(x, y, false);
        if outcome != RemoveOutcome::None {
            self.record_step(BuildStep::Remove {
                x,
                y,
                whole_wall: false,
            });
        }
        outcome
    }

    pub fn remove_whole_wall_at(&mut self, x: i32, y: i32) -> RemoveOutcome {
        let outcome = self.remove_at_with_wall_behavior(x, y, true);
        if outcome != RemoveOutcome::None {
            self.record_step(BuildStep::Remove {
                x,
                y,
                whole_wall: true,
            });
        }
        outcome
    }

    fn remove_at_with_wall_behavior(
        &mut self,
        x: i32,
        y: i32,
        remove_whole_wall: bool,
    ) -> RemoveOutcome {
        if let Some((target_id, goods_yard_group_id)) = self
            .buildings
            .iter()
            .find(|building| building.occupied_cells().any(|cell| cell == (x, y)))
            .map(|building| (building.id, building.goods_yard_group_id))
        {
            let removed_ids = if let Some(group_id) = goods_yard_group_id {
                self.remove_buildings_by_group(group_id)
            } else {
                self.remove_buildings_by_ids([target_id])
            };

            return RemoveOutcome::Buildings {
                removed_ids,
                goods_yard_group_id,
            };
        }

        if remove_whole_wall
            && let Some(index) = self
                .walls
                .iter()
                .rposition(|wall| wall_contains_cell(wall, x, y))
        {
            let id = self.remove_wall_by_index(index);
            return RemoveOutcome::Wall { id };
        }

        let affected_wall_ids = self.remove_wall_cell(x, y);
        if !affected_wall_ids.is_empty() {
            return RemoveOutcome::WallCell { affected_wall_ids };
        }

        RemoveOutcome::None
    }

    pub fn remove_all_walls(&mut self) -> usize {
        let count = self.walls.len();
        if count == 0 {
            return 0;
        }

        for wall in self.walls.drain(..) {
            self.map.clear_wall_cells(wall.cells());
        }

        self.refresh_unavailable_entry_points();
        self.recompute_distances();
        self.record_step(BuildStep::RemoveAllWalls);
        count
    }

    pub fn remove_all(&mut self) -> (usize, usize) {
        let building_count = self.buildings.len();
        let wall_count = self.walls.len();

        for building in self.buildings.drain(..) {
            self.map.clear_cells(building.occupied_cells());
        }
        for wall in self.walls.drain(..) {
            self.map.clear_wall_cells(wall.cells());
        }

        self.distances.clear();
        self.worker_distances.clear();

        self.bread_capacities.get_mut().take();

        // Пустая карта: журнал снова полный, запись возобновляется.
        self.build_log.clear();
        self.record_build_log = true;

        (building_count, wall_count)
    }

    pub fn tick(&mut self, delta_ticks: u64) {
        self.time.advance(delta_ticks);
    }

    pub fn calculate_worker_distances(&mut self) -> usize {
        self.distances = recompute_building_distances(&self.buildings, &self.map);
        self.recompute_worker_distances();
        self.worker_distances.len()
    }

    pub fn set_defer_path_calculation(&mut self, defer: bool) {
        self.defer_path_calculation = defer;
        if defer {
            self.distances.clear();
            self.worker_distances.clear();
        }
    }

    pub fn calculate_bread_economy(&self, settings: SimulationSettings) -> BreadEconomyReport {
        let fear = super::clamped_fear_factor(settings.fear_factor);
        let mut cache = self.bread_capacities.borrow_mut();
        if cache.as_ref().is_some_and(|entry| entry.0 != fear) {
            *cache = None;
        }
        let (_, capacities) = cache.get_or_insert_with(|| {
            (
                fear,
                super::bread::calculate_capacities(
                    &self.map,
                    &self.buildings,
                    &self.distances,
                    fear,
                ),
            )
        });
        calculate_bread_economy(capacities, settings)
    }

    pub fn calculate_production_cycle(
        &self,
        weapon_type: WeaponType,
        workshop_id: u32,
        armoury_id: u32,
        settings: SimulationSettings,
    ) -> Result<ProductionCycle, ProductionCycleError> {
        let recipe = weapon_type.recipe();
        let workshop = find_building(&self.buildings, workshop_id)
            .ok_or(ProductionCycleError::WorkshopNotFound { workshop_id })?;
        let armoury = find_building(&self.buildings, armoury_id)
            .ok_or(ProductionCycleError::ArmouryNotFound { armoury_id })?;

        if workshop.building_type != recipe.workshop_type {
            return Err(ProductionCycleError::ExpectedWorkshop {
                workshop_id,
                actual_type: workshop.building_type,
                expected_type: recipe.workshop_type,
            });
        }

        if armoury.building_type != BuildingType::Armoury {
            return Err(ProductionCycleError::ExpectedArmoury {
                armoury_id,
                actual_type: armoury.building_type,
            });
        }

        let resource = if recipe.wood_required > 0 {
            StockpileResource::Wood
        } else {
            StockpileResource::Iron
        };
        let input = find_stockpile_for_resource(&self.buildings, resource)
            .ok_or(ProductionCycleError::MissingStockpile { resource })?;
        let mut route_usage = Vec::new();
        let required = recipe.total_required_units();
        let ordinary_fletcher = recipe.workshop_type == BuildingType::FletchersWorkshop
            && !settings.optimized_fletcher_routing;
        if ordinary_fletcher {
            self.push_route_usage(
                &mut route_usage,
                recipe.workshop_type,
                armoury_id,
                workshop_id,
                1,
            )?;
            self.push_route_usage(
                &mut route_usage,
                recipe.workshop_type,
                workshop_id,
                input.id,
                required,
            )?;
        } else {
            self.push_route_usage(
                &mut route_usage,
                recipe.workshop_type,
                armoury_id,
                input.id,
                1,
            )?;
            self.push_route_usage(
                &mut route_usage,
                recipe.workshop_type,
                workshop_id,
                input.id,
                required - 1,
            )?;
        }
        self.push_route_usage(
            &mut route_usage,
            recipe.workshop_type,
            input.id,
            workshop_id,
            required,
        )?;
        self.push_route_usage(
            &mut route_usage,
            recipe.workshop_type,
            workshop_id,
            armoury_id,
            1,
        )?;
        let ticks = |from, to| {
            distance_cells(&self.worker_distances, from, to)
                .map(|distance| travel_ticks_for_distance(recipe.workshop_type, distance))
        };
        let paths = super::cycle_timing::WorkshopPaths {
            home_to_input: ticks(workshop_id, input.id)?,
            input_to_home: ticks(input.id, workshop_id)?,
            home_to_output: ticks(workshop_id, armoury_id)?,
            output_to_input: ticks(armoury_id, input.id)?,
            output_to_home: ticks(armoury_id, workshop_id)?,
        };
        let timing = super::cycle_timing::workshop_cycle(weapon_type, paths, settings);
        Ok(ProductionCycle::from_route_usage(
            recipe,
            workshop_id,
            armoury_id,
            route_usage,
            timing,
        ))
    }

    pub fn set_stockpile_resource_at(
        &mut self,
        x: i32,
        y: i32,
        resource: StockpileResource,
    ) -> Result<u32, SimulatorError> {
        let Some(target_id) = self
            .buildings
            .iter()
            .find(|building| {
                building.building_type == BuildingType::Stockpile
                    && building.occupied_cells().any(|cell| cell == (x, y))
            })
            .map(|building| building.id)
        else {
            return Err(SimulatorError::StockpileDesignationRequiresStockpile);
        };

        for building in &mut self.buildings {
            if building.stockpile_resource == Some(resource) {
                building.stockpile_resource = None;
            }
        }

        let target = self
            .buildings
            .iter_mut()
            .find(|building| building.id == target_id)
            .expect("target stockpile should still exist");
        target.stockpile_resource = Some(resource);
        self.recompute_worker_distances();
        self.record_step(BuildStep::Stockpile { x, y, resource });

        Ok(target_id)
    }

    /// Toggle the anchored flag on a building occupying the given cell.
    /// Returns `Ok(true)` if the flag was flipped, `Ok(false)` if no building was found.
    pub fn toggle_anchor_at(&mut self, x: i32, y: i32) -> Result<bool, SimulatorError> {
        let Some(target) = self.buildings.iter_mut().find(|building| {
            building.occupied_cells().any(|cell| cell == (x, y))
        }) else {
            return Ok(false);
        };
        target.anchored = !target.anchored;
        Ok(true)
    }

    /// Returns true if any building is anchored.
    pub fn has_anchors(&self) -> bool {
        self.buildings.iter().any(|b| b.anchored)
    }

    /// Последовательность действий, которыми построена текущая расстановка.
    pub fn build_log(&self) -> &[BuildStep] {
        &self.build_log
    }

    /// Подменяет журнал (загрузка проекта) и включает запись дальше.
    pub fn set_build_log(&mut self, steps: Vec<BuildStep>) {
        self.build_log = steps;
        self.record_build_log = true;
    }

    /// Выключает и очищает журнал (оптимизатор клонирует симулятор тысячи раз).
    pub fn disable_build_log(&mut self) {
        self.build_log.clear();
        self.record_build_log = false;
    }

    fn record_step(&mut self, step: BuildStep) {
        if self.record_build_log {
            self.build_log.push(step);
        }
    }

    /// Применяет один шаг журнала; ошибки игнорируются (шаг мог устареть).
    pub fn apply_build_step(&mut self, step: &BuildStep) {
        match step {
            BuildStep::Building { building_type, x, y } => {
                let _ = self.place_building(*building_type, *x, *y);
            }
            BuildStep::Wall {
                start_x,
                start_y,
                end_x,
                end_y,
            } => {
                let _ = self.place_wall(*start_x, *start_y, *end_x, *end_y);
            }
            BuildStep::Remove {
                x,
                y,
                whole_wall: true,
            } => {
                self.remove_whole_wall_at(*x, *y);
            }
            BuildStep::Remove { x, y, .. } => {
                self.remove_at(*x, *y);
            }
            BuildStep::RemoveAllWalls => {
                self.remove_all_walls();
            }
            BuildStep::Stockpile { x, y, resource } => {
                let _ = self.set_stockpile_resource_at(*x, *y, *resource);
            }
        }
    }

    /// Строит расстановку заново из первых `steps` шагов журнала (на тех же границах карты).
    /// Расстояния не считаются, это только для показа на карте.
    pub fn replay_build_log(&self, steps: usize) -> Option<Simulator> {
        let mut replay = Simulator::with_bounds(self.map_bounds()).ok()?;
        replay.record_build_log = false;
        replay.set_defer_path_calculation(true);
        for step in self.build_log.iter().take(steps) {
            replay.apply_build_step(step);
        }
        Some(replay)
    }

    fn assign_entry_points(&self, placement: &mut BuildingPlacement) {
        placement.entry_point = calculate_building_entry(
            &self.map,
            &self.walls,
            placement.building_type,
            placement.x,
            placement.y,
            placement.width(),
        );

        for component in &mut placement.components {
            component.entry_point = resolve_entry_point_for_square(
                &self.map,
                component.x,
                component.y,
                component.size,
                0,
            );
        }
    }

    fn ensure_bounds_for_extent(
        &mut self,
        min_x: i32,
        min_y: i32,
        max_x: i32,
        max_y: i32,
    ) -> Result<(), SimulatorError> {
        const EXPANSION: i32 = 50;

        let mut bounds = self.map.bounds();
        while i64::from(min_x) - i64::from(bounds.min_x) < i64::from(EXPANSION) {
            bounds.min_x = bounds
                .min_x
                .checked_sub(EXPANSION)
                .ok_or(SimulatorError::InvalidMapSize)?;
        }
        while i64::from(min_y) - i64::from(bounds.min_y) < i64::from(EXPANSION) {
            bounds.min_y = bounds
                .min_y
                .checked_sub(EXPANSION)
                .ok_or(SimulatorError::InvalidMapSize)?;
        }
        while i64::from(bounds.max_x) - i64::from(max_x) < i64::from(EXPANSION) {
            bounds.max_x = bounds
                .max_x
                .checked_add(EXPANSION)
                .ok_or(SimulatorError::InvalidMapSize)?;
        }
        while i64::from(bounds.max_y) - i64::from(max_y) < i64::from(EXPANSION) {
            bounds.max_y = bounds
                .max_y
                .checked_add(EXPANSION)
                .ok_or(SimulatorError::InvalidMapSize)?;
        }

        if bounds == self.map.bounds() {
            return Ok(());
        }

        let mut expanded_map =
            CellMap::with_bounds(bounds).ok_or(SimulatorError::InvalidMapSize)?;
        for building in &self.buildings {
            expanded_map.place(building)?;
        }
        for wall in &self.walls {
            expanded_map.place_wall_cells(wall.cells())?;
        }
        self.map = expanded_map;
        Ok(())
    }

    /// Checks every building and stack, preserving entrances with a walkable local exit.
    pub fn refresh_unavailable_entry_points(&mut self) {
        for index in 0..self.buildings.len() {
            let (building_entry, component_entries) = {
                let building = &self.buildings[index];
                let building_entry = if is_entry_point_available(&self.map, building.entry_point) {
                    building.entry_point
                } else {
                    calculate_building_entry(
                        &self.map,
                        &self.walls,
                        building.building_type,
                        building.x,
                        building.y,
                        building.width(),
                    )
                };
                let component_entries = building
                    .components()
                    .iter()
                    .map(|component| {
                        if is_entry_point_available(&self.map, component.entry_point) {
                            component.entry_point
                        } else {
                            resolve_entry_point_for_square(
                                &self.map,
                                component.x,
                                component.y,
                                component.size,
                                0,
                            )
                        }
                    })
                    .collect::<Vec<_>>();
                (building_entry, component_entries)
            };

            let building = &mut self.buildings[index];
            building.entry_point = building_entry;
            for (component, new_entry) in building.components.iter_mut().zip(component_entries) {
                component.entry_point = new_entry;
            }
        }
    }

    fn remove_buildings_by_group(&mut self, group_id: u32) -> Vec<u32> {
        let ids = self
            .buildings
            .iter()
            .filter(|building| building.goods_yard_group_id == Some(group_id))
            .map(|building| building.id)
            .collect::<Vec<_>>();
        self.remove_buildings_by_ids(ids)
    }

    fn remove_buildings_by_ids<I>(&mut self, ids: I) -> Vec<u32>
    where
        I: IntoIterator<Item = u32>,
    {
        let id_set: HashSet<u32> = ids.into_iter().collect();
        if id_set.is_empty() {
            return Vec::new();
        }

        let mut kept = Vec::with_capacity(self.buildings.len());
        let mut removed_ids = Vec::new();

        for building in self.buildings.drain(..) {
            if id_set.contains(&building.id) {
                self.map.clear_cells(building.occupied_cells());
                removed_ids.push(building.id);
            } else {
                kept.push(building);
            }
        }

        self.buildings = kept;

        if !removed_ids.is_empty() {
            self.refresh_unavailable_entry_points();
            self.recompute_distances();
        }

        removed_ids
    }

    fn remove_wall_by_index(&mut self, index: usize) -> u32 {
        let wall = self.walls.remove(index);
        self.map.clear_wall_cells(wall.cells());
        self.refresh_unavailable_entry_points();
        self.recompute_distances();
        wall.id
    }

    fn remove_wall_cell(&mut self, x: i32, y: i32) -> Vec<u32> {
        let mut affected_wall_ids = Vec::new();
        let mut updated_walls = Vec::with_capacity(self.walls.len() + 1);

        for wall in std::mem::take(&mut self.walls) {
            if !wall_contains_cell(&wall, x, y) {
                updated_walls.push(wall);
                continue;
            }

            affected_wall_ids.push(wall.id);
            self.map.clear_wall_cells([(x, y)]);

            let mut pieces = wall_pieces_without_cell(wall, x, y);
            if let Some(first) = pieces.first_mut() {
                first.id = wall.id;
            }
            if pieces.len() == 2 {
                pieces[1].id = self.next_wall_id;
                self.next_wall_id += 1;
            }
            updated_walls.extend(pieces);
        }

        if affected_wall_ids.is_empty() {
            self.walls = updated_walls;
            return affected_wall_ids;
        }

        self.walls = updated_walls;
        self.refresh_unavailable_entry_points();
        self.recompute_distances();
        affected_wall_ids
    }

    fn recompute_distances(&mut self) {
        self.bread_capacities.get_mut().take();
        if self.defer_path_calculation {
            self.distances.clear();
            self.worker_distances.clear();
            return;
        }
        self.distances = recompute_building_distances(&self.buildings, &self.map);
        self.recompute_worker_distances();
    }

    fn recompute_worker_distances(&mut self) {
        self.bread_capacities.get_mut().take();
        self.worker_distances = build_worker_distances(&self.buildings, &self.distances);
    }

    fn push_route_usage(
        &self,
        route_usage: &mut Vec<ProductionRouteUsage>,
        workshop_type: BuildingType,
        start_building_id: u32,
        finish_building_id: u32,
        trips: u32,
    ) -> Result<(), ProductionCycleError> {
        if trips == 0 {
            return Ok(());
        }

        let distance_cells = distance_cells(
            &self.worker_distances,
            start_building_id,
            finish_building_id,
        )?;
        let total_distance_cells = distance_cells * trips;
        let total_ticks =
            travel_ticks_for_distance(workshop_type, distance_cells) * u64::from(trips);

        if let Some(existing) = route_usage.iter_mut().find(|usage| {
            usage.start_building_id == start_building_id
                && usage.finish_building_id == finish_building_id
        }) {
            existing.trips += trips;
            existing.total_distance_cells += total_distance_cells;
            existing.total_ticks += total_ticks;
            return Ok(());
        }

        route_usage.push(ProductionRouteUsage {
            start_building_id,
            finish_building_id,
            trips,
            distance_cells_per_trip: distance_cells,
            total_distance_cells,
            total_ticks,
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        MapBounds, MapError, WallSegment,
        buildings::{BuildingType, EntryPoint, StockpileResource},
    };

    use super::{
        DistanceKey, RemoveOutcome, SimulationSettings, Simulator, SimulatorError, WeaponType,
    };

    #[test]
    fn bread_capacity_cache_tracks_resource_changes_recalculation_and_clear() {
        let mut simulator = Simulator::new(60).expect("map");
        simulator
            .place_building(BuildingType::WheatFarm, 2, 2)
            .expect("farm");
        simulator
            .place_building(BuildingType::GoodsYard, 20, 20)
            .expect("stock");
        let settings = SimulationSettings::default();
        assert_eq!(
            simulator.calculate_bread_economy(settings).wheat_per_minute,
            0.0
        );
        simulator
            .set_stockpile_resource_at(20, 20, StockpileResource::Wheat)
            .expect("mark wheat");
        let first = simulator.calculate_bread_economy(settings);
        assert!(first.wheat_per_minute > 0.0);
        simulator.set_defer_path_calculation(true);
        simulator.calculate_worker_distances();
        assert_eq!(simulator.calculate_bread_economy(settings), first);
        simulator.remove_all();
        assert_eq!(
            simulator.calculate_bread_economy(settings).wheat_per_minute,
            0.0
        );
    }

    #[test]
    fn places_workshop_when_space_is_free() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        let result = simulator.place_building(BuildingType::FletchersWorkshop, 2, 3);
        assert!(result.is_ok());
        assert_eq!(simulator.buildings().len(), 1);
        assert_eq!(
            simulator.buildings()[0].entry_point,
            Some(EntryPoint { x: 4, y: 2 })
        );
    }

    #[test]
    fn calculates_complete_bread_economy_with_traversable_wheat_field() {
        let mut simulator = Simulator::new(60).expect("simulator should be created");
        simulator
            .place_building(BuildingType::WheatFarm, 2, 2)
            .expect("wheat farm should be placed");
        simulator
            .place_building(BuildingType::Windmill, 14, 3)
            .expect("wind mill should be placed");
        simulator
            .place_building(BuildingType::GoodsYard, 20, 20)
            .expect("goods yard should be placed");
        simulator
            .set_stockpile_resource_at(20, 20, StockpileResource::Wheat)
            .expect("wheat stockpile should be assigned");
        simulator
            .set_stockpile_resource_at(23, 20, StockpileResource::Flour)
            .expect("flour stockpile should be assigned");
        simulator
            .place_building(BuildingType::Bakery, 30, 10)
            .expect("bakery should be placed");
        simulator
            .place_building(BuildingType::Granary, 40, 10)
            .expect("granary should be placed");

        let farm = simulator
            .buildings()
            .iter()
            .find(|building| building.building_type == BuildingType::WheatFarm)
            .expect("wheat farm should exist");
        assert_eq!(farm.entry_point, Some(EntryPoint { x: 6, y: 5 }));

        let report = simulator.calculate_bread_economy(SimulationSettings::default());
        assert!(report.issues.is_empty(), "{:?}", report.issues);
        assert_eq!(report.wheat_per_farm_cycle, 24.0);
        assert_eq!(report.bread_per_flour, 8.0);
        assert!(report.wheat_per_minute > 0.0);
        assert!(report.flour_per_minute > 0.0);
        assert!(report.bread_per_minute > 0.0);
        assert_eq!(report.farm_rates.len(), 1);
        assert_eq!(report.mill_rates.len(), 1);
        assert_eq!(report.bakery_rates.len(), 1);
        assert_eq!(
            report.farm_rates[0].actual_per_minute,
            report.wheat_per_minute
        );
        assert_eq!(
            report.mill_rates[0].actual_per_minute,
            report.flour_per_minute
        );
        assert_eq!(
            report.bakery_rates[0].actual_per_minute,
            report.bread_batches_per_minute
        );
        assert!(
            (report.surplus_wheat_per_minute - (report.wheat_per_minute - report.flour_per_minute))
                .abs()
                < f64::EPSILON
        );
        assert!(
            (report.surplus_flour_per_minute
                - (report.flour_per_minute - report.bread_batches_per_minute))
                .abs()
                < f64::EPSILON
        );
    }

    #[test]
    fn mill_entry_can_use_a_traversable_wheat_field_cell() {
        let mut simulator = Simulator::new(30).expect("simulator should be created");
        simulator
            .place_building(BuildingType::WheatFarm, 5, 5)
            .expect("wheat farm should be placed");
        simulator
            .place_building(BuildingType::Windmill, 9, 14)
            .expect("wind mill should be placed");

        let mill = simulator
            .buildings()
            .iter()
            .find(|building| building.building_type == BuildingType::Windmill)
            .expect("wind mill should exist");
        assert_eq!(mill.entry_point, Some(EntryPoint { x: 10, y: 13 }));
    }

    #[test]
    fn bought_wheat_or_flour_can_supply_bakeries_without_farms() {
        let mut simulator = Simulator::new(60).expect("simulator should be created");
        simulator
            .place_building(BuildingType::Windmill, 14, 3)
            .expect("wind mill should be placed");
        simulator
            .place_building(BuildingType::GoodsYard, 20, 20)
            .expect("goods yard should be placed");
        simulator
            .set_stockpile_resource_at(20, 20, StockpileResource::Wheat)
            .expect("wheat stockpile should be assigned");
        simulator
            .set_stockpile_resource_at(23, 20, StockpileResource::Flour)
            .expect("flour stockpile should be assigned");
        simulator
            .place_building(BuildingType::Bakery, 30, 10)
            .expect("bakery should be placed");
        simulator
            .place_building(BuildingType::Granary, 40, 10)
            .expect("granary should be placed");

        let without_buying = simulator.calculate_bread_economy(SimulationSettings::default());
        assert_eq!(without_buying.bread_per_minute, 0.0);

        let bought_wheat = simulator.calculate_bread_economy(SimulationSettings {
            buy_wheat: true,
            ..SimulationSettings::default()
        });
        assert!(bought_wheat.purchased_wheat_per_minute > 0.0);
        assert_eq!(bought_wheat.purchased_flour_per_minute, 0.0);
        assert!(bought_wheat.bread_per_minute > 0.0);

        let bought_flour = simulator.calculate_bread_economy(SimulationSettings {
            buy_flour: true,
            ..SimulationSettings::default()
        });
        assert_eq!(bought_flour.purchased_wheat_per_minute, 0.0);
        assert!(bought_flour.purchased_flour_per_minute > 0.0);
        assert!(bought_flour.bread_per_minute > 0.0);
    }

    #[test]
    fn bakery_uses_workshop_wall_orientation_rules() {
        let mut simulator = Simulator::new(30).expect("simulator should be created");
        simulator
            .place_wall(14, 10, 14, 13)
            .expect("wall should be placed");
        simulator
            .place_building(BuildingType::Bakery, 10, 10)
            .expect("bakery should be placed");

        assert_eq!(
            simulator.buildings()[0].entry_point,
            Some(EntryPoint { x: 9, y: 11 })
        );
    }

    #[test]
    fn rejects_overlap() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        simulator
            .place_building(BuildingType::Armoury, 5, 5)
            .expect("first placement should succeed");

        let second = simulator.place_building(BuildingType::BlacksmithsWorkshop, 6, 6);
        assert!(second.is_err());
    }

    #[test]
    fn keeps_at_least_fifty_cells_between_a_building_and_every_boundary() {
        let mut simulator = Simulator::new(10).expect("simulator should be created");
        simulator
            .place_building(BuildingType::ArmourersWorkshop, 8, 8)
            .expect("placement should expand the canvas");

        assert_eq!(
            simulator.map_bounds(),
            MapBounds {
                min_x: -50,
                min_y: -50,
                max_x: 110,
                max_y: 110,
            }
        );
        let building = &simulator.buildings()[0];
        let bounds = simulator.map_bounds();
        assert!(building.x - bounds.min_x >= 50);
        assert!(building.y - bounds.min_y >= 50);
        assert!(bounds.max_x - (building.x + building.width() as i32) >= 50);
        assert!(bounds.max_y - (building.y + building.height() as i32) >= 50);
    }

    #[test]
    fn expands_when_a_building_is_close_to_but_not_touching_a_boundary() {
        let mut simulator = Simulator::new(100).expect("simulator should be created");
        simulator
            .place_building(BuildingType::Armoury, 40, 40)
            .expect("placement should succeed");

        assert_eq!(
            simulator.map_bounds(),
            MapBounds {
                min_x: -50,
                min_y: -50,
                max_x: 100,
                max_y: 100,
            }
        );
    }

    #[test]
    fn expands_left_and_bottom_and_accepts_negative_coordinates() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        simulator
            .place_building(BuildingType::Armoury, 0, 0)
            .expect("edge placement should expand the canvas");
        simulator
            .place_building(BuildingType::Bakery, -50, -50)
            .expect("negative placement should expand the canvas again");

        assert_eq!(simulator.map_bounds().min_x, -100);
        assert_eq!(simulator.map_bounds().min_y, -100);
        assert!(simulator.is_cell_occupied(-50, -50));
    }

    #[test]
    fn failed_placement_does_not_commit_canvas_expansion() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        let before = simulator.map_bounds();

        let result = simulator.place_wall(-100, -100, 100, 100);

        assert!(result.is_err());
        assert_eq!(simulator.map_bounds(), before);
    }

    #[test]
    fn goods_yard_has_cross_gap() {
        let mut simulator = Simulator::new(12).expect("simulator should be created");
        simulator
            .place_building(BuildingType::GoodsYard, 1, 1)
            .expect("goods yard should be placed");

        assert!(!simulator.is_cell_occupied(3, 3));
        assert!(simulator.is_cell_occupied(1, 1));
        assert!(simulator.is_cell_occupied(5, 5));
    }

    #[test]
    fn goods_yard_has_four_internal_stacks() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        simulator
            .place_building(BuildingType::GoodsYard, 10, 10)
            .expect("goods yard should be placed");

        let stockpiles: Vec<_> = simulator
            .buildings()
            .iter()
            .filter(|entry| entry.building_type == BuildingType::Stockpile)
            .collect();
        assert_eq!(stockpiles.len(), 4);

        let positions: Vec<(i32, i32)> = stockpiles.iter().map(|s| (s.x, s.y)).collect();
        assert!(positions.contains(&(10, 10)));
        assert!(positions.contains(&(13, 10)));
        assert!(positions.contains(&(10, 13)));
        assert!(positions.contains(&(13, 13)));

        let group_id = stockpiles[0]
            .goods_yard_group_id
            .expect("goods yard stockpile should have group id");
        assert!(
            stockpiles
                .iter()
                .all(|stack| stack.goods_yard_group_id == Some(group_id))
        );
    }

    #[test]
    fn places_horizontal_wall() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        let wall_id = simulator
            .place_wall(2, 4, 6, 4)
            .expect("wall placement should succeed");
        assert_eq!(wall_id, 1);
        assert_eq!(simulator.walls().len(), 1);
        assert!(simulator.is_cell_occupied(2, 4));
        assert!(simulator.is_cell_occupied(6, 4));
    }

    #[test]
    fn walls_can_cross_and_keep_the_intersection_until_both_are_removed() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        let horizontal_id = simulator
            .place_wall(2, 5, 8, 5)
            .expect("horizontal wall should be placed");
        let vertical_id = simulator
            .place_wall(5, 2, 5, 8)
            .expect("vertical wall should cross the horizontal wall");

        assert!(simulator.is_cell_occupied(5, 5));
        assert_eq!(
            simulator.remove_whole_wall_at(5, 5),
            RemoveOutcome::Wall { id: vertical_id }
        );
        assert!(simulator.is_cell_occupied(5, 5));
        assert_eq!(
            simulator.remove_whole_wall_at(5, 5),
            RemoveOutcome::Wall { id: horizontal_id }
        );
        assert!(!simulator.is_cell_occupied(5, 5));
    }

    #[test]
    fn overlapping_wall_cells_remain_until_every_covering_segment_is_removed() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        let first_id = simulator
            .place_wall(2, 5, 7, 5)
            .expect("first wall should be placed");
        let second_id = simulator
            .place_wall(5, 5, 10, 5)
            .expect("overlapping wall should be placed");

        assert_eq!(
            simulator.remove_whole_wall_at(6, 5),
            RemoveOutcome::Wall { id: second_id }
        );
        assert!(simulator.is_cell_occupied(6, 5));
        assert_eq!(
            simulator.remove_whole_wall_at(6, 5),
            RemoveOutcome::Wall { id: first_id }
        );
        assert!(!simulator.is_cell_occupied(6, 5));
    }

    #[test]
    fn removing_a_middle_wall_cell_splits_the_segment() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        simulator
            .place_wall(2, 4, 6, 4)
            .expect("wall should be placed");

        let outcome = simulator.remove_at(4, 4);
        assert_eq!(
            outcome,
            RemoveOutcome::WallCell {
                affected_wall_ids: vec![1]
            }
        );
        assert_eq!(simulator.walls().len(), 2);
        assert_eq!(simulator.walls()[0], WallSegment::new(1, 2, 4, 3, 4));
        assert_eq!(simulator.walls()[1], WallSegment::new(2, 5, 4, 6, 4));
        assert!(simulator.is_cell_occupied(3, 4));
        assert!(!simulator.is_cell_occupied(4, 4));
        assert!(simulator.is_cell_occupied(5, 4));
    }

    #[test]
    fn removing_a_wall_endpoint_shortens_the_segment() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        simulator
            .place_wall(2, 4, 6, 4)
            .expect("wall should be placed");

        assert!(matches!(
            simulator.remove_at(2, 4),
            RemoveOutcome::WallCell { .. }
        ));
        assert_eq!(simulator.walls(), &[WallSegment::new(1, 3, 4, 6, 4)]);
        assert!(!simulator.is_cell_occupied(2, 4));
        assert!(simulator.is_cell_occupied(3, 4));
    }

    #[test]
    fn removing_a_single_wall_cell_deletes_its_segment() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        simulator
            .place_wall(4, 4, 4, 4)
            .expect("one-cell wall should be placed");

        assert!(matches!(
            simulator.remove_at(4, 4),
            RemoveOutcome::WallCell { .. }
        ));
        assert!(simulator.walls().is_empty());
        assert!(!simulator.is_cell_occupied(4, 4));
    }

    #[test]
    fn removing_an_intersection_cell_splits_every_wall_through_it() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        simulator
            .place_wall(2, 5, 8, 5)
            .expect("horizontal wall should be placed");
        simulator
            .place_wall(5, 2, 5, 8)
            .expect("vertical wall should be placed");

        let outcome = simulator.remove_at(5, 5);
        assert_eq!(
            outcome,
            RemoveOutcome::WallCell {
                affected_wall_ids: vec![1, 2]
            }
        );
        assert_eq!(simulator.walls().len(), 4);
        assert!(!simulator.is_cell_occupied(5, 5));
        assert!(simulator.is_cell_occupied(4, 5));
        assert!(simulator.is_cell_occupied(6, 5));
        assert!(simulator.is_cell_occupied(5, 4));
        assert!(simulator.is_cell_occupied(5, 6));
    }

    #[test]
    fn walls_can_cross_walls_but_not_buildings() {
        let mut simulator = Simulator::new(30).expect("simulator should be created");
        simulator
            .place_wall(2, 10, 12, 10)
            .expect("first wall should be placed");
        simulator
            .place_wall(7, 5, 7, 15)
            .expect("crossing wall should be placed");
        simulator
            .place_building(BuildingType::Armoury, 16, 8)
            .expect("building should be placed");

        let result = simulator.place_wall(14, 10, 22, 10);
        assert!(matches!(
            result,
            Err(SimulatorError::Map(MapError::Occupied))
        ));
    }

    #[test]
    fn rejects_diagonal_wall() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        let result = simulator.place_wall(1, 1, 3, 2);
        assert!(matches!(result, Err(SimulatorError::InvalidWallDirection)));
    }

    #[test]
    fn assigns_default_entry_point_for_square_building() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        simulator
            .place_building(BuildingType::Armoury, 6, 6)
            .expect("building should be placed");

        let placed = &simulator.buildings()[0];
        assert_eq!(placed.entry_point, Some(EntryPoint { x: 8, y: 5 }));
    }

    #[test]
    fn rotates_clockwise_when_default_entry_is_blocked() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        simulator
            .place_building(BuildingType::Armoury, 8, 2)
            .expect("blocking building should be placed");
        simulator
            .place_building(BuildingType::Armoury, 6, 6)
            .expect("target building should be placed");

        let target = simulator
            .buildings()
            .iter()
            .find(|b| b.x == 6 && b.y == 6)
            .expect("target building should exist");
        assert_eq!(target.entry_point, Some(EntryPoint { x: 7, y: 5 }));
    }

    #[test]
    fn goods_yard_stacks_receive_individual_entry_points() {
        let mut simulator = Simulator::new(30).expect("simulator should be created");
        simulator
            .place_building(BuildingType::GoodsYard, 10, 10)
            .expect("goods yard should be placed");
        let stockpiles: Vec<_> = simulator
            .buildings()
            .iter()
            .filter(|entry| entry.building_type == BuildingType::Stockpile)
            .collect();
        assert_eq!(stockpiles.len(), 4);
        assert!(stockpiles.iter().all(|stack| stack.entry_point.is_some()));
    }

    #[test]
    fn recalculates_existing_entry_point_when_new_building_blocks_it() {
        let mut simulator = Simulator::new(30).expect("simulator should be created");
        simulator
            .place_building(BuildingType::Armoury, 6, 6)
            .expect("first building should be placed");
        let initial_entry = simulator.buildings()[0]
            .entry_point
            .expect("entry point should exist");
        assert_eq!(initial_entry, EntryPoint { x: 8, y: 5 });

        simulator
            .place_building(BuildingType::Armoury, 8, 2)
            .expect("second building should be placed");

        let updated_entry = simulator
            .buildings()
            .iter()
            .find(|b| b.x == 6 && b.y == 6)
            .expect("first building should still exist")
            .entry_point
            .expect("entry point should still exist after recalculation");
        assert_eq!(updated_entry, EntryPoint { x: 7, y: 5 });
    }

    #[test]
    fn enclosed_workshop_skips_empty_trapped_gap_and_uses_open_corner() {
        let mut simulator = Simulator::new(40).expect("map");
        // Screenshot layout: three solid sides, one empty bottom tile, and a free upper corner.
        for (sx, sy, ex, ey) in [
            (9, 14, 13, 14),
            (9, 8, 9, 13),
            (14, 8, 14, 13),
            (9, 8, 14, 8),
            (10, 9, 11, 9),
            (13, 9, 13, 9),
        ] {
            simulator.place_wall(sx, sy, ex, ey).expect("wall");
        }
        simulator
            .place_building(BuildingType::FletchersWorkshop, 10, 10)
            .expect("workshop");
        assert_eq!(
            simulator.buildings()[0].entry_point,
            Some(EntryPoint { x: 14, y: 14 })
        );

        simulator.place_wall(12, 9, 12, 9).expect("fill gap");
        assert_eq!(
            simulator.buildings()[0].entry_point,
            Some(EntryPoint { x: 14, y: 14 })
        );

        simulator
            .place_wall(14, 14, 14, 14)
            .expect("close final corner");
        assert_eq!(simulator.buildings()[0].entry_point, None);

        simulator.remove_at(14, 14);
        assert_eq!(
            simulator.buildings()[0].entry_point,
            Some(EntryPoint { x: 14, y: 14 })
        );
    }

    #[test]
    fn sealing_workshop_corridor_reroutes_both_entries_to_reachable_corners() {
        for block_with_building in [false, true] {
            let mut simulator = Simulator::new(40).expect("map");
            simulator.set_defer_path_calculation(true);
            let right_x = if block_with_building { 16 } else { 15 };
            let a = simulator
                .place_building(BuildingType::FletchersWorkshop, 10, 10)
                .expect("left workshop");
            let b = simulator
                .place_building(BuildingType::FletchersWorkshop, right_x, 10)
                .expect("right workshop");
            let armoury = simulator
                .place_building(BuildingType::Armoury, 25, 20)
                .expect("armoury");
            let stock = simulator
                .place_building(BuildingType::GoodsYard, 2, 20)
                .expect("yard");
            simulator
                .set_stockpile_resource_at(2, 20, StockpileResource::Wood)
                .expect("wood");
            for (sx, sy, ex, ey) in [
                (9, 9, right_x + 4, 9),
                (9, 10, 9, 13),
                (right_x + 4, 10, right_x + 4, 13),
                (10, 14, 13, 14),
                (right_x, 14, right_x + 3, 14),
            ] {
                simulator.place_wall(sx, sy, ex, ey).expect("wall");
            }
            let original_entries = [
                simulator.buildings()[0].entry_point,
                simulator.buildings()[1].entry_point,
            ];
            let source_entries = simulator.buildings()[2..]
                .iter()
                .map(|b| b.entry_point)
                .collect::<Vec<_>>();
            simulator.calculate_worker_distances();
            assert!(
                simulator
                    .distance_between(a, stock)
                    .expect("route")
                    .distance_cells
                    .is_some()
            );
            assert!(
                simulator
                    .distance_between(b, armoury)
                    .expect("route")
                    .distance_cells
                    .is_some()
            );

            if block_with_building {
                simulator
                    .place_building(BuildingType::Stockpile, 14, 14)
                    .expect("seal with stockpile");
            } else {
                simulator
                    .place_wall(14, 14, 14, 14)
                    .expect("seal with one wall cell");
            }
            for entry in original_entries.into_iter().flatten() {
                assert!(
                    !simulator.map.is_blocked(entry.x, entry.y),
                    "old entrance tile itself is still empty"
                );
                let stock_entry = simulator
                    .buildings()
                    .iter()
                    .find(|b| b.id == stock)
                    .and_then(|b| b.entry_point)
                    .expect("stock entrance");
                assert_eq!(
                    crate::simulation::pathfinding::shortest_path_len(
                        &simulator.map,
                        entry,
                        stock_entry
                    ),
                    None
                );
            }
            assert_eq!(
                simulator.buildings()[0].entry_point,
                Some(EntryPoint { x: 9, y: 14 })
            );
            assert_eq!(
                simulator.buildings()[1].entry_point,
                Some(EntryPoint {
                    x: right_x + 4,
                    y: 14
                })
            );
            assert_eq!(
                simulator.buildings()[2..2 + source_entries.len()]
                    .iter()
                    .map(|b| b.entry_point)
                    .collect::<Vec<_>>(),
                source_entries
            );
            simulator.calculate_worker_distances();
            for id in [a, b] {
                for destination in [stock, armoury] {
                    assert!(
                        simulator
                            .distance_between(id, destination)
                            .expect("route")
                            .distance_cells
                            .is_some()
                    );
                }
            }
        }
    }

    #[test]
    fn reopening_one_diagonal_cell_restores_entrance_for_every_square_building() {
        for kind in [
            BuildingType::Stockpile,
            BuildingType::Windmill,
            BuildingType::Armoury,
            BuildingType::Granary,
            BuildingType::Bakery,
            BuildingType::FletchersWorkshop,
            BuildingType::BlacksmithsWorkshop,
            BuildingType::PoleturnersWorkshop,
            BuildingType::ArmourersWorkshop,
        ] {
            let mut simulator = Simulator::new(40).expect("map");
            simulator.place_building(kind, 10, 10).expect("building");
            let size = simulator.buildings()[0].width() as i32;
            for (sx, sy, ex, ey) in [
                (9, 9, 10 + size, 9),
                (9, 10 + size, 10 + size, 10 + size),
                (9, 10, 9, 9 + size),
                (10 + size, 10, 10 + size, 9 + size),
            ] {
                simulator.place_wall(sx, sy, ex, ey).expect("wall");
            }
            assert_eq!(simulator.buildings()[0].entry_point, None);
            simulator.remove_at(9, 10 + size);
            assert_eq!(
                simulator.buildings()[0].entry_point,
                Some(EntryPoint { x: 9, y: 10 + size })
            );
        }
    }

    #[test]
    fn removing_blocking_building_restores_previously_unavailable_workshop() {
        let mut simulator = Simulator::new(40).expect("map");
        simulator
            .place_building(BuildingType::FletchersWorkshop, 10, 10)
            .expect("workshop");
        let left = simulator
            .place_building(BuildingType::Armoury, 6, 10)
            .expect("left blocker");
        for (x, y) in [(14, 10), (10, 6), (10, 14)] {
            simulator
                .place_building(BuildingType::Armoury, x, y)
                .expect("blocker");
        }
        for (x, y) in [(9, 9), (14, 9), (9, 14), (14, 14)] {
            simulator.place_wall(x, y, x, y).expect("corner wall");
        }
        assert_eq!(simulator.buildings()[0].entry_point, None);
        assert!(simulator.remove_building(left));
        assert_eq!(
            simulator.buildings()[0].entry_point,
            Some(EntryPoint { x: 9, y: 10 })
        );
    }

    #[test]
    fn loading_layout_rechecks_blocked_saved_entries_and_preserves_open_ones() {
        let mut simulator = Simulator::new(40).expect("map");
        simulator
            .place_building(BuildingType::FletchersWorkshop, 10, 10)
            .expect("workshop");
        simulator
            .place_building(BuildingType::Armoury, 25, 25)
            .expect("armoury");
        let buildings = simulator
            .buildings()
            .iter()
            .map(crate::SavedBuilding::from)
            .collect();
        let unaffected = simulator.buildings()[1].entry_point;
        let restored = Simulator::from_saved_layout(
            simulator.map_bounds(),
            buildings,
            vec![crate::WallSegment::new(1, 12, 9, 12, 9)],
        )
        .expect("saved layout");
        assert_eq!(
            restored.buildings()[0].entry_point,
            Some(EntryPoint { x: 11, y: 9 })
        );
        assert_eq!(restored.buildings()[1].entry_point, unaffected);
    }

    #[test]
    fn partial_wall_does_not_rotate_workshops() {
        for kind in [
            BuildingType::FletchersWorkshop,
            BuildingType::BlacksmithsWorkshop,
            BuildingType::PoleturnersWorkshop,
            BuildingType::ArmourersWorkshop,
            BuildingType::Bakery,
        ] {
            for wall in [(14, 10, 14, 12), (10, 9, 11, 9), (9, 10, 9, 12)] {
                let mut simulator = Simulator::new(40).expect("map");
                simulator
                    .place_wall(wall.0, wall.1, wall.2, wall.3)
                    .expect("wall");
                simulator.place_building(kind, 10, 10).expect("workshop");
                assert_eq!(
                    simulator.buildings()[0].entry_point,
                    Some(EntryPoint { x: 12, y: 9 })
                );
            }
        }
    }

    #[test]
    fn full_wall_rotates_then_searches_past_blocked_entry() {
        let mut simulator = Simulator::new(40).expect("map");
        // Partial contact on the right must not override the complete upper wall.
        simulator.place_wall(14, 10, 14, 12).expect("partial wall");
        for x in 10..14 {
            simulator.place_wall(x, 9, x, 9).expect("wall cell");
        }
        simulator
            .place_building(BuildingType::Armoury, 11, 14)
            .expect("blocker");
        simulator
            .place_building(BuildingType::FletchersWorkshop, 10, 10)
            .expect("workshop");
        assert_eq!(
            simulator.buildings()[1].entry_point,
            Some(EntryPoint { x: 14, y: 13 })
        );
    }

    #[test]
    fn wall_cell_rechecks_blocked_entries_for_all_square_buildings() {
        for kind in [
            BuildingType::Stockpile,
            BuildingType::Windmill,
            BuildingType::Armoury,
            BuildingType::Granary,
            BuildingType::Bakery,
            BuildingType::FletchersWorkshop,
            BuildingType::BlacksmithsWorkshop,
            BuildingType::PoleturnersWorkshop,
            BuildingType::ArmourersWorkshop,
        ] {
            let mut simulator = Simulator::new(40).expect("map");
            simulator.place_building(kind, 10, 10).expect("building");
            simulator
                .place_building(BuildingType::Armoury, 25, 25)
                .expect("unaffected building");
            let entry = simulator.buildings()[0].entry_point.expect("entry");
            let unaffected = simulator.buildings()[1].entry_point;
            simulator
                .place_wall(entry.x, entry.y, entry.x, entry.y)
                .expect("wall cell");
            let updated = simulator.buildings()[0]
                .entry_point
                .expect("replacement entry");
            assert_eq!(
                updated,
                EntryPoint {
                    x: if kind == BuildingType::Stockpile {
                        9
                    } else {
                        entry.x - 1
                    },
                    y: if kind == BuildingType::Stockpile {
                        10
                    } else {
                        9
                    }
                }
            );
            assert!(!simulator.map.is_blocked(updated.x, updated.y));
            assert_eq!(simulator.buildings()[1].entry_point, unaffected);
        }
    }

    #[test]
    fn workshop_entry_rotates_with_wall_orientation() {
        let cases = [
            ((10, 14, 13, 14), EntryPoint { x: 12, y: 9 }),
            ((14, 10, 14, 13), EntryPoint { x: 9, y: 11 }),
            ((10, 9, 13, 9), EntryPoint { x: 11, y: 14 }),
            ((9, 10, 9, 13), EntryPoint { x: 14, y: 12 }),
        ];

        for ((start_x, start_y, end_x, end_y), expected_entry) in cases {
            let mut simulator = Simulator::new(40).expect("simulator should be created");
            simulator
                .place_wall(start_x, start_y, end_x, end_y)
                .expect("wall should be placed");
            simulator
                .place_building(BuildingType::FletchersWorkshop, 10, 10)
                .expect("workshop should be placed");

            let workshop = simulator
                .buildings()
                .iter()
                .find(|b| b.x == 10 && b.y == 10)
                .expect("workshop should exist");

            assert_eq!(workshop.entry_point, Some(expected_entry));
        }
    }

    #[test]
    fn stores_directional_distance_objects_for_both_directions() {
        let mut simulator = Simulator::new(30).expect("simulator should be created");
        let a = simulator
            .place_building(BuildingType::FletchersWorkshop, 2, 3)
            .expect("building A should be placed");
        let b = simulator
            .place_building(BuildingType::Armoury, 10, 3)
            .expect("building B should be placed");

        assert!(simulator.distances().contains_key(&DistanceKey::new(a, b)));
        assert!(simulator.distances().contains_key(&DistanceKey::new(b, a)));
    }

    #[test]
    fn walls_can_enclose_a_building_and_make_distance_unreachable() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        let start_id = simulator
            .place_building(BuildingType::FletchersWorkshop, 5, 5)
            .expect("start building should be placed");
        let finish_id = simulator
            .place_building(BuildingType::Armoury, 12, 5)
            .expect("finish building should be placed");

        simulator
            .place_wall(4, 4, 9, 4)
            .expect("bottom wall should be placed");
        simulator
            .place_wall(4, 9, 9, 9)
            .expect("top wall should be placed");
        simulator
            .place_wall(4, 5, 4, 8)
            .expect("left wall should be placed");
        simulator
            .place_wall(9, 5, 9, 8)
            .expect("right wall should be placed");

        let distance = simulator
            .distance_between(start_id, finish_id)
            .expect("distance object should exist");
        assert_eq!(distance.distance_cells, None);
    }

    #[test]
    fn removing_one_stockpile_removes_whole_goods_yard_group() {
        let mut simulator = Simulator::new(30).expect("simulator should be created");
        simulator
            .place_building(BuildingType::GoodsYard, 10, 10)
            .expect("goods yard should be placed");

        let outcome = simulator.remove_at(10, 10);
        match outcome {
            RemoveOutcome::Buildings {
                removed_ids,
                goods_yard_group_id,
            } => {
                assert_eq!(removed_ids.len(), 4);
                assert!(goods_yard_group_id.is_some());
            }
            _ => panic!("expected building removal"),
        }

        assert!(
            simulator
                .buildings()
                .iter()
                .all(|building| building.building_type != BuildingType::Stockpile)
        );
        assert!(!simulator.is_cell_occupied(10, 10));
        assert!(!simulator.is_cell_occupied(13, 10));
        assert!(!simulator.is_cell_occupied(10, 13));
        assert!(!simulator.is_cell_occupied(13, 13));
    }

    #[test]
    fn remove_all_walls_clears_wall_cells() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        simulator
            .place_wall(2, 4, 6, 4)
            .expect("wall placement should succeed");
        simulator
            .place_wall(8, 2, 8, 5)
            .expect("wall placement should succeed");

        let removed = simulator.remove_all_walls();
        assert_eq!(removed, 2);
        assert_eq!(simulator.walls().len(), 0);
        assert!(!simulator.is_cell_occupied(2, 4));
        assert!(!simulator.is_cell_occupied(8, 5));
    }

    #[test]
    fn remove_all_clears_buildings_walls_and_distances() {
        let mut simulator = Simulator::new(30).expect("simulator should be created");
        simulator
            .place_building(BuildingType::Armoury, 2, 2)
            .expect("armoury should be placed");
        simulator
            .place_building(BuildingType::FletchersWorkshop, 10, 2)
            .expect("workshop should be placed");
        simulator
            .place_wall(2, 10, 6, 10)
            .expect("wall should be placed");

        let removed = simulator.remove_all();

        assert_eq!(removed, (2, 1));
        assert!(simulator.buildings().is_empty());
        assert!(simulator.walls().is_empty());
        assert!(simulator.distances().is_empty());
        assert!(simulator.worker_distances().is_empty());
        assert!(!simulator.is_cell_occupied(2, 2));
        assert!(!simulator.is_cell_occupied(2, 10));
    }

    #[test]
    fn removing_wall_does_not_recalculate_workshop_entry_point() {
        let mut simulator = Simulator::new(40).expect("simulator should be created");
        simulator
            .place_wall(14, 10, 14, 13)
            .expect("wall should be placed");
        let workshop_id = simulator
            .place_building(BuildingType::FletchersWorkshop, 10, 10)
            .expect("workshop should be placed");

        let before = simulator
            .buildings()
            .iter()
            .find(|b| b.id == workshop_id)
            .expect("workshop should exist")
            .entry_point;
        assert_eq!(before, Some(EntryPoint { x: 9, y: 11 }));

        let removed = simulator.remove_all_walls();
        assert_eq!(removed, 1);

        let after = simulator
            .buildings()
            .iter()
            .find(|b| b.id == workshop_id)
            .expect("workshop should exist")
            .entry_point;
        assert_eq!(after, Some(EntryPoint { x: 9, y: 11 }));
    }

    #[test]
    fn removing_wall_assigns_entry_only_when_building_had_none() {
        let mut simulator = Simulator::new(30).expect("simulator should be created");
        simulator
            .place_wall(5, 5, 10, 5)
            .expect("bottom wall should be placed");
        simulator
            .place_wall(5, 10, 10, 10)
            .expect("top wall should be placed");
        simulator
            .place_wall(5, 6, 5, 9)
            .expect("left wall should be placed");
        simulator
            .place_wall(10, 6, 10, 9)
            .expect("right wall should be placed");

        let armoury_id = simulator
            .place_building(BuildingType::Armoury, 6, 6)
            .expect("armoury should be placed inside blocked perimeter");
        let before = simulator
            .buildings()
            .iter()
            .find(|b| b.id == armoury_id)
            .expect("armoury should exist")
            .entry_point;
        assert_eq!(before, None);

        let outcome = simulator.remove_at(8, 5);
        assert!(matches!(outcome, RemoveOutcome::WallCell { .. }));

        let after = simulator
            .buildings()
            .iter()
            .find(|b| b.id == armoury_id)
            .expect("armoury should exist")
            .entry_point;
        assert_eq!(after, Some(EntryPoint { x: 8, y: 5 }));
    }

    #[test]
    fn removing_building_does_not_recalculate_other_entry_points() {
        let mut simulator = Simulator::new(30).expect("simulator should be created");
        let target_id = simulator
            .place_building(BuildingType::Armoury, 6, 6)
            .expect("target building should be placed");
        let blocker_id = simulator
            .place_building(BuildingType::Armoury, 8, 2)
            .expect("blocking building should be placed");

        let before = simulator
            .buildings()
            .iter()
            .find(|b| b.id == target_id)
            .expect("target should exist")
            .entry_point;
        assert_eq!(before, Some(EntryPoint { x: 7, y: 5 }));

        assert!(simulator.remove_building(blocker_id));

        let after = simulator
            .buildings()
            .iter()
            .find(|b| b.id == target_id)
            .expect("target should exist")
            .entry_point;
        assert_eq!(after, Some(EntryPoint { x: 7, y: 5 }));
    }

    #[test]
    fn stockpile_resource_moves_between_stockpiles() {
        let mut simulator = Simulator::new(30).expect("simulator should be created");
        simulator
            .place_building(BuildingType::GoodsYard, 10, 10)
            .expect("goods yard should be placed");

        let first_id = simulator
            .set_stockpile_resource_at(10, 10, StockpileResource::Wood)
            .expect("first stockpile should accept wood");
        let second_id = simulator
            .set_stockpile_resource_at(13, 10, StockpileResource::Wood)
            .expect("second stockpile should accept wood");

        assert_ne!(first_id, second_id);
        assert_eq!(
            simulator
                .buildings()
                .iter()
                .find(|b| b.id == first_id)
                .expect("first stockpile should exist")
                .stockpile_resource,
            None
        );
        assert_eq!(
            simulator
                .buildings()
                .iter()
                .find(|b| b.id == second_id)
                .expect("second stockpile should exist")
                .stockpile_resource,
            Some(StockpileResource::Wood)
        );
    }

    #[test]
    fn stockpile_cannot_hold_wood_and_iron_together() {
        let mut simulator = Simulator::new(30).expect("simulator should be created");
        simulator
            .place_building(BuildingType::GoodsYard, 10, 10)
            .expect("goods yard should be placed");

        let stockpile_id = simulator
            .set_stockpile_resource_at(10, 10, StockpileResource::Wood)
            .expect("stockpile should accept wood");
        simulator
            .set_stockpile_resource_at(10, 10, StockpileResource::Iron)
            .expect("stockpile should switch to iron");

        assert_eq!(
            simulator
                .buildings()
                .iter()
                .find(|b| b.id == stockpile_id)
                .expect("stockpile should exist")
                .stockpile_resource,
            Some(StockpileResource::Iron)
        );
    }

    #[test]
    fn stockpile_designation_rejects_non_stockpile_cells() {
        let mut simulator = Simulator::new(30).expect("simulator should be created");
        simulator
            .place_building(BuildingType::Armoury, 5, 5)
            .expect("armoury should be placed");

        let result = simulator.set_stockpile_resource_at(5, 5, StockpileResource::Wood);
        assert!(matches!(
            result,
            Err(SimulatorError::StockpileDesignationRequiresStockpile)
        ));
    }

    #[test]
    fn calculates_worker_distances_for_wood_workshop_and_armoury_routes() {
        let mut simulator = Simulator::new(40).expect("simulator should be created");
        simulator
            .place_building(BuildingType::GoodsYard, 2, 2)
            .expect("goods yard should be placed");
        simulator
            .set_stockpile_resource_at(2, 2, StockpileResource::Wood)
            .expect("wood stockpile should be marked");
        let workshop_id = simulator
            .place_building(BuildingType::FletchersWorkshop, 10, 2)
            .expect("fletchers workshop should be placed");
        let armoury_id = simulator
            .place_building(BuildingType::Armoury, 18, 2)
            .expect("armoury should be placed");
        let wood_stockpile_id = simulator
            .buildings()
            .iter()
            .find(|building| building.stockpile_resource == Some(StockpileResource::Wood))
            .expect("wood stockpile should exist")
            .id;

        let count = simulator.calculate_worker_distances();
        assert_eq!(count, 5);
        assert!(
            simulator
                .worker_distance_between(workshop_id, wood_stockpile_id)
                .is_some()
        );
        assert!(
            simulator
                .worker_distance_between(wood_stockpile_id, workshop_id)
                .is_some()
        );
        assert!(
            simulator
                .worker_distance_between(workshop_id, armoury_id)
                .is_some()
        );
        assert!(
            simulator
                .worker_distance_between(armoury_id, workshop_id)
                .is_some()
        );
        assert!(
            simulator
                .worker_distance_between(armoury_id, wood_stockpile_id)
                .is_some()
        );
    }

    #[test]
    fn calculates_worker_distances_for_iron_routes() {
        let mut simulator = Simulator::new(40).expect("simulator should be created");
        simulator
            .place_building(BuildingType::GoodsYard, 2, 2)
            .expect("goods yard should be placed");
        simulator
            .set_stockpile_resource_at(2, 2, StockpileResource::Iron)
            .expect("iron stockpile should be marked");
        let workshop_id = simulator
            .place_building(BuildingType::BlacksmithsWorkshop, 10, 2)
            .expect("blacksmiths workshop should be placed");
        let armoury_id = simulator
            .place_building(BuildingType::Armoury, 18, 2)
            .expect("armoury should be placed");
        let iron_stockpile_id = simulator
            .buildings()
            .iter()
            .find(|building| building.stockpile_resource == Some(StockpileResource::Iron))
            .expect("iron stockpile should exist")
            .id;

        simulator.calculate_worker_distances();

        assert!(
            simulator
                .worker_distance_between(workshop_id, iron_stockpile_id)
                .is_some()
        );
        assert!(
            simulator
                .worker_distance_between(iron_stockpile_id, workshop_id)
                .is_some()
        );
        assert!(
            simulator
                .worker_distance_between(armoury_id, iron_stockpile_id)
                .is_some()
        );
    }

    #[test]
    fn deferred_path_calculation_waits_for_explicit_calculate() {
        let mut simulator = Simulator::new(40).expect("simulator should be created");
        simulator.set_defer_path_calculation(true);
        simulator
            .place_building(BuildingType::GoodsYard, 2, 2)
            .expect("goods yard should be placed");
        simulator
            .place_building(BuildingType::FletchersWorkshop, 10, 2)
            .expect("workshop should be placed");
        simulator
            .place_building(BuildingType::Armoury, 18, 2)
            .expect("armoury should be placed");

        assert!(simulator.distances().is_empty());
        assert!(simulator.worker_distances().is_empty());

        assert!(simulator.calculate_worker_distances() > 0);
        assert!(!simulator.distances().is_empty());
        assert!(!simulator.worker_distances().is_empty());
    }

    #[test]
    fn deferred_mode_keeps_large_layout_edits_free_of_path_recalculation() {
        let mut simulator = Simulator::new(200).expect("simulator should be created");
        simulator.set_defer_path_calculation(true);

        for index in 0..120 {
            let x = 5 + (index % 20) * 7;
            let y = 5 + (index / 20) * 7;
            simulator
                .place_building(BuildingType::Armoury, x, y)
                .expect("spaced armoury should be placed");
        }

        assert_eq!(simulator.buildings().len(), 120);
        assert!(simulator.distances().is_empty());
        assert!(simulator.worker_distances().is_empty());
    }

    #[test]
    fn bow_cycle_starts_from_armoury_and_returns_to_armoury() {
        let mut simulator = Simulator::new(40).expect("simulator should be created");
        simulator
            .place_building(BuildingType::GoodsYard, 2, 2)
            .expect("goods yard should be placed");
        simulator
            .set_stockpile_resource_at(2, 2, StockpileResource::Wood)
            .expect("wood stockpile should be marked");
        let workshop_id = simulator
            .place_building(BuildingType::FletchersWorkshop, 10, 2)
            .expect("fletchers workshop should be placed");
        let armoury_id = simulator
            .place_building(BuildingType::Armoury, 18, 2)
            .expect("armoury should be placed");
        let wood_stockpile_id = simulator
            .buildings()
            .iter()
            .find(|building| building.stockpile_resource == Some(StockpileResource::Wood))
            .expect("wood stockpile should exist")
            .id;

        let cycle = simulator
            .calculate_production_cycle(
                WeaponType::Bow,
                workshop_id,
                armoury_id,
                SimulationSettings::default(),
            )
            .expect("bow cycle should be calculated");

        assert_eq!(cycle.recipe.wood_required, 2);
        assert_eq!(cycle.route_usage.len(), 4);
        assert_eq!(cycle.route_usage[0].start_building_id, armoury_id);
        assert_eq!(cycle.route_usage[0].finish_building_id, workshop_id);
        assert_eq!(cycle.route_usage[0].trips, 1);
        assert_eq!(cycle.route_usage[1].start_building_id, workshop_id);
        assert_eq!(cycle.route_usage[1].finish_building_id, wood_stockpile_id);
        assert_eq!(cycle.route_usage[1].trips, 2);
        assert_eq!(cycle.route_usage[2].start_building_id, wood_stockpile_id);
        assert_eq!(cycle.route_usage[2].finish_building_id, workshop_id);
        assert_eq!(cycle.route_usage[2].trips, 2);
        assert_eq!(cycle.route_usage[3].start_building_id, workshop_id);
        assert_eq!(cycle.route_usage[3].finish_building_id, armoury_id);
        assert_eq!(cycle.route_usage[3].trips, 1);
        assert_eq!(cycle.total_ticks, cycle.travel_ticks + cycle.work_ticks);
    }

    #[test]
    fn optimized_fletcher_cycle_goes_directly_from_armoury_to_stockpile() {
        let mut simulator = Simulator::new(40).expect("simulator should be created");
        simulator
            .place_building(BuildingType::GoodsYard, 2, 2)
            .expect("goods yard should be placed");
        simulator
            .set_stockpile_resource_at(2, 2, StockpileResource::Wood)
            .expect("wood stockpile should be marked");
        let workshop_id = simulator
            .place_building(BuildingType::FletchersWorkshop, 10, 2)
            .expect("fletchers workshop should be placed");
        let armoury_id = simulator
            .place_building(BuildingType::Armoury, 18, 2)
            .expect("armoury should be placed");
        let wood_stockpile_id = simulator
            .buildings()
            .iter()
            .find(|building| building.stockpile_resource == Some(StockpileResource::Wood))
            .expect("wood stockpile should exist")
            .id;

        let cycle = simulator
            .calculate_production_cycle(
                WeaponType::Bow,
                workshop_id,
                armoury_id,
                SimulationSettings {
                    optimized_fletcher_routing: true,
                    ..SimulationSettings::default()
                },
            )
            .expect("optimized bow cycle should be calculated");

        assert_eq!(cycle.route_usage.len(), 4);
        assert_eq!(cycle.route_usage[0].start_building_id, armoury_id);
        assert_eq!(cycle.route_usage[0].finish_building_id, wood_stockpile_id);
        assert_eq!(cycle.route_usage[0].trips, 1);
        assert_eq!(cycle.route_usage[1].start_building_id, workshop_id);
        assert_eq!(cycle.route_usage[1].finish_building_id, wood_stockpile_id);
        assert_eq!(cycle.route_usage[1].trips, 1);
        assert_eq!(cycle.route_usage[2].start_building_id, wood_stockpile_id);
        assert_eq!(cycle.route_usage[2].finish_building_id, workshop_id);
        assert_eq!(cycle.route_usage[2].trips, 2);
        assert_eq!(cycle.route_usage[3].start_building_id, workshop_id);
        assert_eq!(cycle.route_usage[3].finish_building_id, armoury_id);
        assert_eq!(cycle.route_usage[3].trips, 1);
    }

    #[test]
    fn spear_cycle_uses_armoury_to_stockpile_to_workshop_pattern() {
        let mut simulator = Simulator::new(40).expect("simulator should be created");
        simulator
            .place_building(BuildingType::GoodsYard, 2, 2)
            .expect("goods yard should be placed");
        simulator
            .set_stockpile_resource_at(2, 2, StockpileResource::Wood)
            .expect("wood stockpile should be marked");
        let workshop_id = simulator
            .place_building(BuildingType::PoleturnersWorkshop, 10, 2)
            .expect("poleturners workshop should be placed");
        let armoury_id = simulator
            .place_building(BuildingType::Armoury, 18, 2)
            .expect("armoury should be placed");
        let wood_stockpile_id = simulator
            .buildings()
            .iter()
            .find(|building| building.stockpile_resource == Some(StockpileResource::Wood))
            .expect("wood stockpile should exist")
            .id;

        let cycle = simulator
            .calculate_production_cycle(
                WeaponType::Spear,
                workshop_id,
                armoury_id,
                SimulationSettings::default(),
            )
            .expect("spear cycle should be calculated");

        assert_eq!(cycle.route_usage.len(), 3);
        assert_eq!(cycle.route_usage[0].start_building_id, armoury_id);
        assert_eq!(cycle.route_usage[0].finish_building_id, wood_stockpile_id);
        assert_eq!(cycle.route_usage[0].trips, 1);
        assert_eq!(cycle.route_usage[1].start_building_id, wood_stockpile_id);
        assert_eq!(cycle.route_usage[1].finish_building_id, workshop_id);
        assert_eq!(cycle.route_usage[1].trips, 1);
        assert_eq!(cycle.route_usage[2].start_building_id, workshop_id);
        assert_eq!(cycle.route_usage[2].finish_building_id, armoury_id);
        assert_eq!(cycle.route_usage[2].trips, 1);
    }
}
