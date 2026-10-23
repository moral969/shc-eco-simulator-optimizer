use std::collections::HashMap;

use crate::{
    CellMap,
    buildings::{BuildingPlacement, BuildingType, StockpileResource},
};

use super::cycle_timing::{bakery_cycle, mill_cycle};
use super::wheat_cycle::wheat_cycle;
use super::{BuildingDistance, CycleTiming, DistanceKey, SimulationSettings};

pub const WHEAT_SELL_GOLD: f64 = 8.0;
pub const WHEAT_BUY_GOLD: f64 = 23.0;
pub const FLOUR_SELL_GOLD: f64 = 10.0;
pub const FLOUR_BUY_GOLD: f64 = 32.0;
pub const BREAD_SELL_GOLD: f64 = 4.0;
pub const BREAD_BUY_GOLD: f64 = 8.0;

#[derive(Debug, Clone, PartialEq)]
pub struct BreadBuildingRate {
    pub building_id: u32,
    pub capacity_per_minute: f64,
    pub actual_per_minute: f64,
    pub timing: CycleTiming,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BreadEconomyReport {
    pub wheat_per_minute: f64,
    pub flour_capacity_per_minute: f64,
    pub flour_per_minute: f64,
    pub bakery_batches_per_minute: f64,
    pub bread_per_minute: f64,
    pub bread_batches_per_minute: f64,
    pub surplus_wheat_per_minute: f64,
    pub surplus_flour_per_minute: f64,
    pub purchased_wheat_per_minute: f64,
    pub purchased_flour_per_minute: f64,
    pub wheat_per_farm_cycle: f64,
    pub bread_per_flour: f64,
    pub limiting_stage: &'static str,
    pub farm_count: usize,
    pub mill_count: usize,
    pub bakery_count: usize,
    pub granary_count: usize,
    pub farm_rates: Vec<BreadBuildingRate>,
    pub mill_rates: Vec<BreadBuildingRate>,
    pub bakery_rates: Vec<BreadBuildingRate>,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone)]
struct Capacity {
    building_id: u32,
    per_tick: f64,
    timing: CycleTiming,
}

#[derive(Debug, Clone, Default)]
pub(super) struct BreadCapacities {
    farms: Vec<Capacity>,
    mills: Vec<Capacity>,
    bakeries: Vec<Capacity>,
    farm_count: usize,
    mill_count: usize,
    bakery_count: usize,
    granary_count: usize,
    issues: Vec<String>,
}

pub(super) fn calculate_capacities(
    map: &CellMap,
    buildings: &[BuildingPlacement],
    distances: &HashMap<DistanceKey, BuildingDistance>,
    fear: i32,
) -> BreadCapacities {
    let farms = buildings_of_type(buildings, BuildingType::WheatFarm);
    let mills = buildings_of_type(buildings, BuildingType::Windmill);
    let bakeries = buildings_of_type(buildings, BuildingType::Bakery);
    let granaries = buildings_of_type(buildings, BuildingType::Granary);
    let wheat_stock = stockpile(buildings, StockpileResource::Wheat);
    let flour_stock = stockpile(buildings, StockpileResource::Flour);
    let mut result = BreadCapacities {
        farm_count: farms.len(),
        mill_count: mills.len(),
        bakery_count: bakeries.len(),
        granary_count: granaries.len(),
        ..BreadCapacities::default()
    };
    if wheat_stock.is_none() {
        result
            .issues
            .push("No stockpile is marked for Wheat".into());
    }
    if flour_stock.is_none() {
        result
            .issues
            .push("No stockpile is marked for Flour".into());
    }
    if granaries.is_empty() {
        result.issues.push("No Granary placed".into());
    }
    if let Some(stock) = wheat_stock {
        for farm in farms {
            if route_distance(distances, farm.id, stock.id, &mut result.issues).is_none() {
                continue;
            }
            let timing = stock
                .entry_point
                .and_then(|entry| wheat_cycle(map, farm, entry, fear));
            if let Some(timing) = timing {
                result.farms.push(Capacity {
                    building_id: farm.id,
                    per_tick: timing.output_per_tick(),
                    timing,
                });
            } else {
                result.issues.push(format!(
                    "Wheat Farm #{} cannot finish its crop/stock routes",
                    farm.id
                ));
            }
        }
    }
    if let (Some(wheat), Some(flour)) = (wheat_stock, flour_stock) {
        for mill in mills {
            let legs = route_distance(distances, flour.id, wheat.id, &mut result.issues)
                .zip(route_distance(
                    distances,
                    wheat.id,
                    mill.id,
                    &mut result.issues,
                ))
                .zip(route_distance(
                    distances,
                    mill.id,
                    flour.id,
                    &mut result.issues,
                ));
            if let Some(((empty, loaded_in), loaded_out)) = legs {
                let timing = mill_cycle(
                    u64::from(empty) * 12,
                    u64::from(loaded_in) * 16,
                    u64::from(loaded_out) * 16,
                );
                result.mills.push(Capacity {
                    building_id: mill.id,
                    per_tick: timing.output_per_tick(),
                    timing,
                });
            }
        }
    }
    if let Some(flour) = flour_stock {
        for bakery in bakeries {
            // Stock/native granary selection uses ascending building slots, not the shortest loop.
            let granary = granaries.iter().min_by_key(|granary| granary.id);
            let legs = granary.and_then(|granary| {
                distance(distances, granary.id, flour.id)
                    .zip(distance(distances, flour.id, bakery.id))
                    .zip(distance(distances, bakery.id, granary.id))
            });
            if let Some(((empty, loaded_in), loaded_out)) = legs {
                let timing = bakery_cycle(
                    u64::from(loaded_in) * 24,
                    u64::from(empty) * 24,
                    u64::from(loaded_out) * 24,
                    fear,
                );
                result.bakeries.push(Capacity {
                    building_id: bakery.id,
                    per_tick: 1.0 / timing.mean_ticks,
                    timing,
                });
            } else {
                result.issues.push(format!(
                    "Bakery #{} has no reachable Flour stockpile and Granary route",
                    bakery.id
                ));
            }
        }
    }
    result
}

pub(super) fn calculate_bread_economy(
    capacities: &BreadCapacities,
    settings: SimulationSettings,
) -> BreadEconomyReport {
    let wheat_per_farm_cycle = wheat_output_per_farm_cycle(settings.fear_factor);
    let bread_per_flour = bread_output_per_flour(settings.fear_factor);
    let farm_rates_per_tick = &capacities.farms;
    let mill_rates_per_tick = &capacities.mills;
    let bakery_rates_per_tick = &capacities.bakeries;
    let wheat_per_tick = sum_rates(farm_rates_per_tick);
    let flour_capacity_per_tick = sum_rates(mill_rates_per_tick);
    let bakery_capacity_per_tick = sum_rates(bakery_rates_per_tick);
    let farm_wheat_processed_per_tick = wheat_per_tick.min(flour_capacity_per_tick);
    let purchased_wheat_per_tick = if settings.buy_wheat {
        (bakery_capacity_per_tick - farm_wheat_processed_per_tick)
            .max(0.0)
            .min((flour_capacity_per_tick - farm_wheat_processed_per_tick).max(0.0))
    } else {
        0.0
    };
    let flour_per_tick = farm_wheat_processed_per_tick + purchased_wheat_per_tick;
    let purchased_flour_per_tick = if settings.buy_flour {
        (bakery_capacity_per_tick - flour_per_tick).max(0.0)
    } else {
        0.0
    };
    let bread_batches_per_tick =
        (flour_per_tick + purchased_flour_per_tick).min(bakery_capacity_per_tick);
    let ticks_per_minute = f64::from(settings.game_speed_ticks_per_second) * 60.0;
    let limiting_stage = limiting_stage(
        wheat_per_tick,
        flour_capacity_per_tick,
        bakery_capacity_per_tick,
        settings.buy_wheat,
        settings.buy_flour,
    );

    BreadEconomyReport {
        wheat_per_minute: wheat_per_tick * ticks_per_minute,
        flour_capacity_per_minute: flour_capacity_per_tick * ticks_per_minute,
        flour_per_minute: flour_per_tick * ticks_per_minute,
        bakery_batches_per_minute: bakery_capacity_per_tick * ticks_per_minute,
        bread_per_minute: bread_batches_per_tick * bread_per_flour * ticks_per_minute,
        bread_batches_per_minute: bread_batches_per_tick * ticks_per_minute,
        surplus_wheat_per_minute: (wheat_per_tick - farm_wheat_processed_per_tick).max(0.0)
            * ticks_per_minute,
        surplus_flour_per_minute: (flour_per_tick - bread_batches_per_tick).max(0.0)
            * ticks_per_minute,
        purchased_wheat_per_minute: purchased_wheat_per_tick * ticks_per_minute,
        purchased_flour_per_minute: purchased_flour_per_tick * ticks_per_minute,
        wheat_per_farm_cycle,
        bread_per_flour,
        limiting_stage,
        farm_count: capacities.farm_count,
        mill_count: capacities.mill_count,
        bakery_count: capacities.bakery_count,
        granary_count: capacities.granary_count,
        farm_rates: allocate_rates(farm_rates_per_tick, wheat_per_tick, ticks_per_minute),
        mill_rates: allocate_rates(mill_rates_per_tick, flour_per_tick, ticks_per_minute),
        bakery_rates: allocate_rates(
            bakery_rates_per_tick,
            bread_batches_per_tick,
            ticks_per_minute,
        ),
        issues: capacities.issues.clone(),
    }
}

fn sum_rates(rates: &[Capacity]) -> f64 {
    rates.iter().map(|rate| rate.per_tick).sum()
}

fn allocate_rates(
    rates: &[Capacity],
    actual_total_per_tick: f64,
    ticks_per_minute: f64,
) -> Vec<BreadBuildingRate> {
    let capacity_total = sum_rates(rates);
    rates
        .iter()
        .map(|capacity| BreadBuildingRate {
            building_id: capacity.building_id,
            timing: capacity.timing,
            capacity_per_minute: capacity.per_tick * ticks_per_minute,
            actual_per_minute: if capacity_total == 0.0 {
                0.0
            } else {
                actual_total_per_tick * (capacity.per_tick / capacity_total) * ticks_per_minute
            },
        })
        .collect()
}

pub fn wheat_output_per_farm_cycle(fear_factor: i32) -> f64 {
    24.0 + fear_progress(fear_factor) * 12.0
}

pub fn bread_output_per_flour(fear_factor: i32) -> f64 {
    8.0 + fear_progress(fear_factor) * 4.0
}

fn fear_progress(fear_factor: i32) -> f64 {
    f64::from(fear_factor.clamp(-5, 0).unsigned_abs()) / 5.0
}

fn buildings_of_type(
    buildings: &[BuildingPlacement],
    building_type: BuildingType,
) -> Vec<&BuildingPlacement> {
    buildings
        .iter()
        .filter(|building| building.building_type == building_type)
        .collect()
}

fn stockpile(
    buildings: &[BuildingPlacement],
    resource: StockpileResource,
) -> Option<&BuildingPlacement> {
    buildings
        .iter()
        .find(|building| building.stockpile_resource == Some(resource))
}

fn route_distance(
    distances: &HashMap<DistanceKey, BuildingDistance>,
    start_id: u32,
    finish_id: u32,
    issues: &mut Vec<String>,
) -> Option<u32> {
    let result = distance(distances, start_id, finish_id);
    if result.is_none() {
        issues.push(format!(
            "Route #{} -> #{} is unreachable",
            start_id, finish_id
        ));
    }
    result
}

fn distance(
    distances: &HashMap<DistanceKey, BuildingDistance>,
    start_id: u32,
    finish_id: u32,
) -> Option<u32> {
    distances
        .get(&DistanceKey::new(start_id, finish_id))?
        .distance_cells
}

fn limiting_stage(
    wheat: f64,
    mill: f64,
    bakery: f64,
    buy_wheat: bool,
    buy_flour: bool,
) -> &'static str {
    if buy_flour {
        return "Bakery";
    }
    if buy_wheat {
        return if mill <= bakery {
            "Wind mill"
        } else {
            "Bakery"
        };
    }
    if wheat <= mill && wheat <= bakery {
        "Wheat"
    } else if mill <= bakery {
        "Wind mill"
    } else {
        "Bakery"
    }
}

#[cfg(test)]
mod tests {
    use super::{bread_output_per_flour, wheat_output_per_farm_cycle};
    #[test]
    fn productivity_changes_mean_integer_yield() {
        assert_eq!(wheat_output_per_farm_cycle(0), 24.0);
        assert_eq!(wheat_output_per_farm_cycle(-5), 36.0);
        assert_eq!(bread_output_per_flour(0), 8.0);
        assert_eq!(bread_output_per_flour(-5), 12.0);
    }
}
