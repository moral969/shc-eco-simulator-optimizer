use std::{sync::mpsc, thread};

use simulator::{
    BuildingType, MapBounds, ProductionCycle, RemoveOutcome, SimulationSettings, Simulator,
    StockpileResource, WeaponType,
};

#[derive(Debug)]
pub enum BackendCommand {
    PlaceBuilding {
        building_type: BuildingType,
        x: i32,
        y: i32,
    },
    PlaceWall {
        start: (i32, i32),
        end: (i32, i32),
    },
    RemoveAt {
        x: i32,
        y: i32,
        whole_wall: bool,
    },
    RemoveAll,
    RemoveAllWalls,
    Undo,
    Redo,
    LoadProject {
        simulator: Box<Simulator>,
        settings: SimulationSettings,
    },
    SetStockpileResource {
        x: i32,
        y: i32,
        resource: StockpileResource,
    },
    RunCycleSimulation {
        settings: SimulationSettings,
    },
    ToggleAnchor {
        x: i32,
        y: i32,
    },
    /// Перенос постройки: (x, y) — любая её клетка, (new_x, new_y) — новый левый нижний угол.
    MoveBuilding {
        x: i32,
        y: i32,
        new_x: i32,
        new_y: i32,
    },
    /// Автопересчёт цикла после каждого изменения геометрии.
    SetAutoRecalculate {
        enabled: bool,
        settings: SimulationSettings,
    },
}

#[derive(Debug, Clone)]
pub struct CycleSimulationRow {
    pub workshop_id: u32,
    pub workshop_name: String,
    pub weapon_type: WeaponType,
    pub armoury_id: Option<u32>,
    pub total_ticks: Option<u64>,
    pub travel_ticks: Option<u64>,
    pub work_ticks: Option<u64>,
    pub timing: Option<simulator::CycleTiming>,
    pub average_weapons_per_cycle: f64,
    pub wood_per_cycle: u32,
    pub iron_per_cycle: u32,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BackendUpdate {
    pub simulator: Simulator,
    pub message: String,
    pub cycle_rows: Option<Vec<CycleSimulationRow>>,
    pub geometry_changed: bool,
    pub cycle_settings: Option<SimulationSettings>,
}

#[derive(Clone)]
pub struct BackendHandle {
    sender: mpsc::Sender<BackendCommand>,
}

impl BackendHandle {
    pub fn spawn(
        map_size: usize,
        on_update: impl Fn(BackendUpdate) + Send + 'static,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (sender, receiver) = mpsc::channel();

        thread::Builder::new()
            .name("simulator-backend".to_string())
            .spawn(move || run_backend(map_size, receiver, on_update))?;

        Ok(Self { sender })
    }

    pub fn send(&self, command: BackendCommand) -> Result<(), String> {
        self.sender
            .send(command)
            .map_err(|_| "backend thread is not available".to_string())
    }
}

fn run_backend(
    map_size: usize,
    receiver: mpsc::Receiver<BackendCommand>,
    on_update: impl Fn(BackendUpdate),
) {
    let mut simulator = match Simulator::new(map_size) {
        Ok(simulator) => simulator,
        Err(error) => {
            eprintln!("failed to start simulator backend: {error}");
            return;
        }
    };
    simulator.set_defer_path_calculation(true);
    let mut history = MapHistory::default();
    let mut paths_stale = true;
    // Some — автопересчёт включён; внутри актуальные настройки цикла.
    let mut auto_settings: Option<SimulationSettings> = None;

    for command in receiver {
        let mut cycle_settings = match &command {
            BackendCommand::LoadProject { settings, .. }
            | BackendCommand::RunCycleSimulation { settings } => Some(*settings),
            BackendCommand::SetAutoRecalculate { enabled, settings } => {
                auto_settings = enabled.then_some(*settings);
                None
            }
            _ => None,
        };
        if auto_settings.is_some()
            && let Some(settings) = cycle_settings
        {
            auto_settings = Some(settings);
        }
        let (message, mut cycle_rows, geometry_changed) = match command {
            BackendCommand::MoveBuilding {
                x,
                y,
                new_x,
                new_y,
            } => {
                simulator.set_defer_path_calculation(true);
                let previous = simulator.clone();
                let previous_bounds = previous.map_bounds();
                let result = move_building(&mut simulator, x, y, new_x, new_y);
                let changed = result.is_ok();
                (
                    match result {
                        Ok(building_type) => {
                            let expansion =
                                canvas_expansion_message(previous_bounds, simulator.map_bounds());
                            history.record(previous);
                            format!(
                                "Moved {} to ({}, {}){}",
                                building_type.display_name(),
                                new_x,
                                new_y,
                                expansion,
                            )
                        }
                        Err(error) => {
                            // Частично применённый перенос откатываем целиком.
                            simulator = previous;
                            format!("Move failed: {}", error)
                        }
                    },
                    None,
                    changed,
                )
            }
            BackendCommand::SetAutoRecalculate { enabled, .. } => (
                if enabled {
                    "Auto recalculation enabled".to_string()
                } else {
                    "Auto recalculation disabled".to_string()
                },
                None,
                false,
            ),
            BackendCommand::PlaceBuilding {
                building_type,
                x,
                y,
            } => {
                simulator.set_defer_path_calculation(true);
                let previous = simulator.clone();
                let previous_bounds = previous.map_bounds();
                let result = simulator.place_building(building_type, x, y);
                let changed = result.is_ok();
                (
                    match result {
                        Ok(id) => {
                            let expansion =
                                canvas_expansion_message(previous_bounds, simulator.map_bounds());
                            history.record(previous);
                            format!(
                                "Placed {} #{} at ({}, {}){}",
                                building_type.display_name(),
                                id,
                                x,
                                y,
                                expansion,
                            )
                        }
                        Err(error) => format!("Placement failed: {}", error),
                    },
                    None,
                    changed,
                )
            }
            BackendCommand::PlaceWall { start, end } => {
                simulator.set_defer_path_calculation(true);
                let previous = simulator.clone();
                let previous_bounds = previous.map_bounds();
                let result = simulator.place_wall(start.0, start.1, end.0, end.1);
                let changed = result.is_ok();
                (
                    match result {
                        Ok(id) => {
                            let expansion =
                                canvas_expansion_message(previous_bounds, simulator.map_bounds());
                            history.record(previous);
                            format!(
                                "Placed Wall #{} from ({}, {}) to ({}, {}){}",
                                id, start.0, start.1, end.0, end.1, expansion,
                            )
                        }
                        Err(error) => format!("Placement failed: {}", error),
                    },
                    None,
                    changed,
                )
            }
            BackendCommand::RemoveAt { x, y, whole_wall } => {
                simulator.set_defer_path_calculation(true);
                let previous = simulator.clone();
                let outcome = if whole_wall {
                    simulator.remove_whole_wall_at(x, y)
                } else {
                    simulator.remove_at(x, y)
                };
                if outcome != RemoveOutcome::None {
                    history.record(previous);
                }
                let changed = outcome != RemoveOutcome::None;
                (
                    match outcome {
                        RemoveOutcome::None => "Nothing to remove at this cell".to_string(),
                        RemoveOutcome::Wall { id } => format!("Removed Wall #{}", id),
                        RemoveOutcome::WallCell { affected_wall_ids } => format!(
                            "Removed wall cell at ({}, {}) from {} segment(s)",
                            x,
                            y,
                            affected_wall_ids.len()
                        ),
                        RemoveOutcome::Buildings {
                            removed_ids,
                            goods_yard_group_id,
                        } => {
                            if removed_ids.is_empty() {
                                "Nothing removed".to_string()
                            } else if let Some(group_id) = goods_yard_group_id {
                                format!(
                                    "Removed Goods Yard group #{} ({} stockpiles)",
                                    group_id,
                                    removed_ids.len()
                                )
                            } else {
                                format!("Removed building #{}", removed_ids[0])
                            }
                        }
                    },
                    None,
                    changed,
                )
            }
            BackendCommand::RemoveAll => {
                simulator.set_defer_path_calculation(true);
                let previous = simulator.clone();
                let (building_count, wall_count) = simulator.remove_all();
                if building_count > 0 || wall_count > 0 {
                    history.record(previous);
                }
                (
                    {
                        if building_count == 0 && wall_count == 0 {
                            "Map is already empty".to_string()
                        } else {
                            format!(
                                "Removed {} building(s) and {} wall segment(s)",
                                building_count, wall_count
                            )
                        }
                    },
                    None,
                    building_count > 0 || wall_count > 0,
                )
            }
            BackendCommand::RemoveAllWalls => {
                simulator.set_defer_path_calculation(true);
                let previous = simulator.clone();
                let removed = simulator.remove_all_walls();
                if removed > 0 {
                    history.record(previous);
                }
                (
                    {
                        if removed == 0 {
                            "No walls to remove".to_string()
                        } else {
                            format!("Removed {} wall segment(s)", removed)
                        }
                    },
                    None,
                    removed > 0,
                )
            }
            BackendCommand::SetStockpileResource { x, y, resource } => {
                simulator.set_defer_path_calculation(true);
                let previous = simulator.clone();
                let result = simulator.set_stockpile_resource_at(x, y, resource);
                let changed = result.is_ok();
                (
                    match result {
                        Ok(id) => {
                            history.record(previous);
                            format!("Marked stockpile #{} as {}", id, resource.display_name())
                        }
                        Err(error) => format!("Placement failed: {}", error),
                    },
                    None,
                    changed,
                )
            }
            BackendCommand::Undo => {
                let changed = history.undo(&mut simulator);
                (
                    if changed {
                        "Undid last map change".to_string()
                    } else {
                        "Nothing to undo".to_string()
                    },
                    None,
                    changed,
                )
            }
            BackendCommand::Redo => {
                let changed = history.redo(&mut simulator);
                (
                    if changed {
                        "Redid map change".to_string()
                    } else {
                        "Nothing to redo".to_string()
                    },
                    None,
                    changed,
                )
            }
            BackendCommand::ToggleAnchor { x, y } => {
                simulator.set_defer_path_calculation(true);
                let previous = simulator.clone();
                let result = simulator.toggle_anchor_at(x, y);
                let changed = matches!(result, Ok(true));
                if changed {
                    history.record(previous);
                }
                (
                    match result {
                        Ok(true) => {
                            let anchored = simulator
                                .buildings()
                                .iter()
                                .find(|b| b.occupied_cells().any(|c| c == (x, y)))
                                .map(|b| b.anchored)
                                .unwrap_or(false);
                            if anchored {
                                format!("Anchored building at ({}, {})", x, y)
                            } else {
                                format!("Unanchored building at ({}, {})", x, y)
                            }
                        }
                        Ok(false) => "No building at this cell".to_string(),
                        Err(error) => format!("Anchor toggle failed: {}", error),
                    },
                    None,
                    changed,
                )
            }
            BackendCommand::LoadProject {
                simulator: loaded_simulator,
                settings,
            } => {
                simulator = *loaded_simulator;
                simulator.set_defer_path_calculation(true);
                history = MapHistory::default();
                simulator.calculate_worker_distances();
                paths_stale = false;
                let cycle_rows = build_cycle_simulation_rows(&simulator, settings);
                simulator.calculate_bread_economy(settings);
                let success_count = cycle_rows
                    .iter()
                    .filter(|row| row.total_ticks.is_some())
                    .count();
                (
                    format!(
                        "Project opened; calculated {} workshop cycle(s), {} ready",
                        cycle_rows.len(),
                        success_count
                    ),
                    Some(cycle_rows),
                    false,
                )
            }
            BackendCommand::RunCycleSimulation { settings } => {
                if paths_stale {
                    simulator.calculate_worker_distances();
                    paths_stale = false;
                }
                let cycle_rows = build_cycle_simulation_rows(&simulator, settings);
                simulator.calculate_bread_economy(settings);
                let success_count = cycle_rows
                    .iter()
                    .filter(|row| row.total_ticks.is_some())
                    .count();
                (
                    format!(
                        "Calculated {} workshop cycle(s), {} ready",
                        cycle_rows.len(),
                        success_count
                    ),
                    Some(cycle_rows),
                    false,
                )
            }
        };

        if geometry_changed {
            simulator.refresh_unavailable_entry_points();
            paths_stale = true;
        }

        // Автопересчёт: после любого изменения построек сразу считаем циклы.
        if geometry_changed
            && let Some(settings) = auto_settings
        {
            simulator.calculate_worker_distances();
            paths_stale = false;
            cycle_rows = Some(build_cycle_simulation_rows(&simulator, settings));
            simulator.calculate_bread_economy(settings);
            cycle_settings = Some(settings);
        }

        on_update(BackendUpdate {
            simulator: simulator.clone(),
            message,
            cycle_rows,
            geometry_changed,
            cycle_settings,
        });
    }
}

/// Переносит постройку через публичное API ядра: удаляет и ставит заново,
/// затем возвращает назначенные ресурсы складов и якоря.
/// Goods Yard переносится целиком (стеки получают новые id).
fn move_building(
    simulator: &mut Simulator,
    x: i32,
    y: i32,
    new_x: i32,
    new_y: i32,
) -> Result<BuildingType, String> {
    let (target_id, group, target_type) = simulator
        .buildings()
        .iter()
        .find(|building| building.occupied_cells().any(|cell| cell == (x, y)))
        .map(|building| (building.id, building.goods_yard_group_id, building.building_type))
        .ok_or("No building at this cell")?;

    // Всё, что нужно восстановить после переноса: (x, y, ресурс, якорь).
    let moved: Vec<(i32, i32, Option<StockpileResource>, bool)> = simulator
        .buildings()
        .iter()
        .filter(|building| match group {
            Some(group_id) => building.goods_yard_group_id == Some(group_id),
            None => building.id == target_id,
        })
        .map(|building| {
            (
                building.x,
                building.y,
                building.stockpile_resource,
                building.anchored,
            )
        })
        .collect();
    let origin = moved
        .iter()
        .fold((i32::MAX, i32::MAX), |acc, m| (acc.0.min(m.0), acc.1.min(m.1)));
    let building_type = if group.is_some() {
        BuildingType::GoodsYard
    } else {
        target_type
    };

    if !simulator.remove_building(target_id) {
        return Err("could not remove building".to_string());
    }
    simulator
        .place_building(building_type, new_x, new_y)
        .map_err(|error| error.to_string())?;

    for (bx, by, resource, anchored) in moved {
        let cell_x = new_x + bx - origin.0;
        let cell_y = new_y + by - origin.1;
        if let Some(resource) = resource {
            simulator
                .set_stockpile_resource_at(cell_x, cell_y, resource)
                .map_err(|error| error.to_string())?;
        }
        if anchored {
            simulator
                .toggle_anchor_at(cell_x, cell_y)
                .map_err(|error| error.to_string())?;
        }
    }

    Ok(building_type)
}

fn canvas_expansion_message(previous: MapBounds, current: MapBounds) -> String {
    if previous == current {
        return String::new();
    }

    format!(
        "; canvas expanded to X {}..{}, Y {}..{}",
        current.min_x,
        current.max_x - 1,
        current.min_y,
        current.max_y - 1,
    )
}

#[derive(Default)]
struct MapHistory {
    undo: Vec<Simulator>,
    redo: Vec<Simulator>,
}

impl MapHistory {
    fn record(&mut self, previous: Simulator) {
        self.undo.push(previous);
        self.redo.clear();
    }

    fn undo(&mut self, simulator: &mut Simulator) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };

        self.redo.push(std::mem::replace(simulator, previous));
        true
    }

    fn redo(&mut self, simulator: &mut Simulator) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };

        self.undo.push(std::mem::replace(simulator, next));
        true
    }
}

fn build_cycle_simulation_rows(
    simulator: &Simulator,
    settings: SimulationSettings,
) -> Vec<CycleSimulationRow> {
    let mut armoury_ids = simulator
        .buildings()
        .iter()
        .filter(|building| building.building_type == BuildingType::Armoury)
        .map(|building| building.id)
        .collect::<Vec<_>>();
    armoury_ids.sort_unstable();

    let mut rows = Vec::new();

    for workshop in simulator.buildings().iter().filter(|building| {
        matches!(
            building.building_type,
            BuildingType::FletchersWorkshop
                | BuildingType::BlacksmithsWorkshop
                | BuildingType::PoleturnersWorkshop
                | BuildingType::ArmourersWorkshop
        )
    }) {
        let Some(weapon_type) = settings.selected_weapon_for(workshop.building_type) else {
            continue;
        };

        rows.push(build_cycle_row(
            simulator,
            workshop.id,
            workshop.building_type,
            &armoury_ids,
            weapon_type,
            settings,
        ));
    }

    rows
}

fn build_cycle_row(
    simulator: &Simulator,
    workshop_id: u32,
    workshop_type: BuildingType,
    armoury_ids: &[u32],
    weapon_type: WeaponType,
    settings: SimulationSettings,
) -> CycleSimulationRow {
    let workshop_name = format!("#{} {}", workshop_id, workshop_type.display_name());
    let recipe = weapon_type.recipe();
    let average_weapons_per_cycle = settings.average_weapon_output_per_cycle(workshop_type);

    if armoury_ids.is_empty() {
        return CycleSimulationRow {
            workshop_id,
            workshop_name,
            weapon_type,
            armoury_id: None,
            total_ticks: None,
            travel_ticks: None,
            work_ticks: None,
            timing: None,
            average_weapons_per_cycle,
            wood_per_cycle: recipe.wood_required,
            iron_per_cycle: recipe.iron_required,
            error: Some("No armoury placed".to_string()),
        };
    }

    let mut best_cycle: Option<ProductionCycle> = None;
    let mut first_error = None;

    for armoury_id in armoury_ids {
        match simulator.calculate_production_cycle(weapon_type, workshop_id, *armoury_id, settings)
        {
            Ok(cycle) => {
                best_cycle = Some(cycle);
                break;
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error.to_string());
                }
            }
        }
    }

    if let Some(cycle) = best_cycle {
        return CycleSimulationRow {
            workshop_id,
            workshop_name,
            weapon_type,
            armoury_id: Some(cycle.armoury_id),
            total_ticks: Some(cycle.total_ticks),
            travel_ticks: Some(cycle.travel_ticks),
            work_ticks: Some(cycle.work_ticks),
            timing: Some(cycle.timing),
            average_weapons_per_cycle: cycle.timing.output_per_cycle,
            wood_per_cycle: recipe.wood_required,
            iron_per_cycle: recipe.iron_required,
            error: None,
        };
    }

    CycleSimulationRow {
        workshop_id,
        workshop_name,
        weapon_type,
        armoury_id: None,
        total_ticks: None,
        travel_ticks: None,
        work_ticks: None,
        timing: None,
        average_weapons_per_cycle,
        wood_per_cycle: recipe.wood_required,
        iron_per_cycle: recipe.iron_required,
        error: Some(first_error.unwrap_or_else(|| "No reachable cycle".to_string())),
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

    use simulator::{BuildingType, EntryPoint, MapBounds, SimulationSettings, Simulator};

    use super::{BackendCommand, BackendHandle, MapHistory};

    #[test]
    fn every_map_update_publishes_available_entrance_markers() {
        let (sender, receiver) = mpsc::channel();
        let backend = BackendHandle::spawn(40, move |update| {
            sender.send(update).expect("receiver");
        })
        .expect("backend");
        let commands = [
            BackendCommand::PlaceBuilding {
                building_type: BuildingType::GoodsYard,
                x: 20,
                y: 20,
            },
            // Occupies the two lower stockpiles' original entrances.
            BackendCommand::PlaceBuilding {
                building_type: BuildingType::Armoury,
                x: 20,
                y: 16,
            },
            BackendCommand::PlaceWall {
                start: (19, 20),
                end: (19, 20),
            },
            BackendCommand::SetStockpileResource {
                x: 20,
                y: 20,
                resource: simulator::StockpileResource::Wood,
            },
            BackendCommand::Undo,
            BackendCommand::Redo,
            BackendCommand::RemoveAt {
                x: 19,
                y: 20,
                whole_wall: false,
            },
            BackendCommand::RemoveAt {
                x: 20,
                y: 16,
                whole_wall: false,
            },
            BackendCommand::PlaceBuilding {
                building_type: BuildingType::PoleturnersWorkshop,
                x: 10,
                y: 10,
            },
            BackendCommand::PlaceWall {
                start: (10, 9),
                end: (13, 9),
            },
            BackendCommand::PlaceWall {
                start: (10, 14),
                end: (13, 14),
            },
        ];
        for (step, command) in commands.into_iter().enumerate() {
            backend.send(command).expect("command");
            let update = receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("update");
            assert!(update.geometry_changed, "failed edit: {}", update.message);
            let blocked = update
                .simulator
                .buildings()
                .iter()
                .flat_map(|building| building.blocking_cells())
                .chain(
                    update
                        .simulator
                        .walls()
                        .iter()
                        .flat_map(|wall| wall.cells()),
                )
                .collect::<std::collections::HashSet<_>>();
            for marker in crate::visuals::build_entry_labels(&update.simulator) {
                assert!(
                    !blocked.contains(&(marker.x, marker.y)),
                    "blocked marker {} after {}",
                    marker.text,
                    update.message
                );
            }
            let stacks = update.simulator.buildings();
            if step == 1 {
                assert_eq!(stacks[0].entry_point, Some(EntryPoint { x: 19, y: 20 }));
                assert_eq!(stacks[1].entry_point, Some(EntryPoint { x: 22, y: 20 }));
            }
            if (2..=7).contains(&step) {
                assert_eq!(stacks[0].entry_point, Some(EntryPoint { x: 19, y: 21 }));
                assert_eq!(stacks[1].entry_point, Some(EntryPoint { x: 22, y: 20 }));
            }
        }
    }

    #[test]
    fn loading_project_immediately_returns_fresh_calculation_rows() {
        let simulator = Simulator::new(20).expect("simulator should be created");
        let settings = SimulationSettings {
            game_speed_ticks_per_second: 65,
            fear_factor: -3,
            ..SimulationSettings::default()
        };
        let (update_sender, update_receiver) = mpsc::channel();
        let backend = BackendHandle::spawn(20, move |update| {
            update_sender
                .send(update)
                .expect("test update receiver should be available");
        })
        .expect("backend should start");

        backend
            .send(BackendCommand::LoadProject {
                simulator: Box::new(simulator),
                settings,
            })
            .expect("load command should be accepted");

        let update = update_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("calculated load update should arrive");
        assert!(update.cycle_rows.is_some());
        assert!(!update.geometry_changed);
        assert!(update.message.contains("Project opened; calculated"));
    }

    #[test]
    fn history_restores_complete_map_snapshots_and_clears_redo_after_edit() {
        let mut simulator = Simulator::new(40).expect("simulator should be created");
        let mut history = MapHistory::default();

        let previous = simulator.clone();
        simulator
            .place_wall(14, 10, 14, 13)
            .expect("wall should be placed");
        history.record(previous);

        let previous = simulator.clone();
        simulator
            .place_building(BuildingType::FletchersWorkshop, 10, 10)
            .expect("workshop should be placed");
        history.record(previous);

        let expected_entry = Some(EntryPoint { x: 9, y: 11 });
        assert_eq!(simulator.buildings()[0].entry_point, expected_entry);

        let previous = simulator.clone();
        simulator.remove_all();
        history.record(previous);

        assert!(history.undo(&mut simulator));
        assert_eq!(simulator.walls().len(), 1);
        assert_eq!(simulator.buildings()[0].entry_point, expected_entry);

        assert!(history.undo(&mut simulator));
        assert_eq!(simulator.walls().len(), 1);
        assert!(simulator.buildings().is_empty());

        assert!(history.redo(&mut simulator));
        assert_eq!(simulator.buildings()[0].entry_point, expected_entry);

        let previous = simulator.clone();
        simulator
            .place_building(BuildingType::Armoury, 20, 20)
            .expect("armoury should be placed");
        history.record(previous);

        assert!(!history.redo(&mut simulator));
    }

    #[test]
    fn history_restores_canvas_expansion_with_the_placement() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        let initial_bounds = simulator.map_bounds();
        let mut history = MapHistory::default();

        let previous = simulator.clone();
        simulator
            .place_building(BuildingType::Armoury, 0, 0)
            .expect("edge placement should succeed");
        history.record(previous);

        assert_eq!(
            simulator.map_bounds(),
            MapBounds {
                min_x: -50,
                min_y: -50,
                max_x: 70,
                max_y: 70,
            }
        );
        assert!(history.undo(&mut simulator));
        assert_eq!(simulator.map_bounds(), initial_bounds);
        assert!(simulator.buildings().is_empty());

        assert!(history.redo(&mut simulator));
        assert_eq!(simulator.map_bounds().min_x, -50);
        assert_eq!(simulator.map_bounds().min_y, -50);
        assert_eq!(simulator.buildings().len(), 1);
    }

    #[test]
    fn history_restores_a_wall_after_pixel_removal() {
        let mut simulator = Simulator::new(20).expect("simulator should be created");
        simulator
            .place_wall(2, 5, 8, 5)
            .expect("wall should be placed");
        let mut history = MapHistory::default();

        let previous = simulator.clone();
        simulator.remove_at(5, 5);
        history.record(previous);
        assert_eq!(simulator.walls().len(), 2);
        assert!(!simulator.is_cell_occupied(5, 5));

        assert!(history.undo(&mut simulator));
        assert_eq!(simulator.walls().len(), 1);
        assert!(simulator.is_cell_occupied(5, 5));

        assert!(history.redo(&mut simulator));
        assert_eq!(simulator.walls().len(), 2);
        assert!(!simulator.is_cell_occupied(5, 5));
    }
}
