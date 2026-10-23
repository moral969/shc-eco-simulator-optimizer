pub mod buildings;
pub mod map;
mod project;
pub mod simulation;
pub mod walls;

pub use buildings::{
    BuildingComponent, BuildingComponentType, BuildingCost, BuildingPlacement, BuildingType,
    EntryPoint, Footprint, StockpileResource, WORKSHOP_SLOWDOWN_BASE, unit_speed_cells_per_tick,
};
pub use map::{CellMap, MapBounds, MapError};
pub use project::{PROJECT_FORMAT_VERSION, ProjectFile, SavedBuilding};
pub use simulation::{
    BREAD_BUY_GOLD, BREAD_SELL_GOLD, BreadBuildingRate, BreadEconomyReport, BuildingDistance,
    CycleTiming, DistanceKey, FLOUR_BUY_GOLD, FLOUR_SELL_GOLD, GameTime, IRON_BUY_GOLD,
    IRON_PER_MINUTE, IronEconomy, PopulationEconomyContext, PopulationEconomyReport,
    PopulationEconomySettings, ProductionCycle, ProductionCycleError, ProductionRouteUsage,
    RemoveOutcome, STONE_SELL_GOLD, SimulationSettings, Simulator, SimulatorError, WHEAT_BUY_GOLD,
    WHEAT_SELL_GOLD, WOOD_BUY_GOLD, WeaponRecipe, WeaponType, bread_output_per_flour,
    calculate_iron_economy, calculate_population_economy, clamped_fear_factor,
    wheat_output_per_farm_cycle,
};
pub use walls::WallSegment;

pub const DEFAULT_MAP_SIZE: usize = 100;
