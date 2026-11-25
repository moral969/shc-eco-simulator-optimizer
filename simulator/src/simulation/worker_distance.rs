use std::collections::HashMap;

use crate::buildings::{BuildingPlacement, BuildingType, StockpileResource};

use super::{BuildingDistance, DistanceKey};

pub(crate) fn build_worker_distances(
    buildings: &[BuildingPlacement],
    distances: &HashMap<DistanceKey, BuildingDistance>,
) -> HashMap<DistanceKey, BuildingDistance> {
    let mut worker_distances = HashMap::new();

    let wood_stockpile_ids = buildings
        .iter()
        .filter(|building| building.stockpile_resource == Some(StockpileResource::Wood))
        .map(|building| building.id)
        .collect::<Vec<_>>();
    let iron_stockpile_ids = buildings
        .iter()
        .filter(|building| building.stockpile_resource == Some(StockpileResource::Iron))
        .map(|building| building.id)
        .collect::<Vec<_>>();
    let armoury_ids = buildings
        .iter()
        .filter(|building| building.building_type == BuildingType::Armoury)
        .map(|building| building.id)
        .collect::<Vec<_>>();

    for workshop in buildings
        .iter()
        .filter(|building| is_workshop(building.building_type))
    {
        let stockpile_ids = match workshop.building_type {
            BuildingType::FletchersWorkshop | BuildingType::PoleturnersWorkshop => {
                &wood_stockpile_ids
            }
            BuildingType::BlacksmithsWorkshop | BuildingType::ArmourersWorkshop => {
                &iron_stockpile_ids
            }
            _ => continue,
        };

        for stockpile_id in stockpile_ids {
            insert_distance(&mut worker_distances, distances, workshop.id, *stockpile_id);
            insert_distance(&mut worker_distances, distances, *stockpile_id, workshop.id);
        }

        for armoury_id in &armoury_ids {
            insert_distance(&mut worker_distances, distances, workshop.id, *armoury_id);
            insert_distance(&mut worker_distances, distances, *armoury_id, workshop.id);
        }
    }

    for armoury_id in &armoury_ids {
        for stockpile_id in &wood_stockpile_ids {
            insert_distance(&mut worker_distances, distances, *armoury_id, *stockpile_id);
        }

        for stockpile_id in &iron_stockpile_ids {
            insert_distance(&mut worker_distances, distances, *armoury_id, *stockpile_id);
        }
    }

    worker_distances
}

fn insert_distance(
    out: &mut HashMap<DistanceKey, BuildingDistance>,
    all_distances: &HashMap<DistanceKey, BuildingDistance>,
    start_building_id: u32,
    finish_building_id: u32,
) {
    let key = DistanceKey::new(start_building_id, finish_building_id);
    if let Some(distance) = all_distances.get(&key) {
        out.insert(key, distance.clone());
    }
}

fn is_workshop(building_type: BuildingType) -> bool {
    matches!(
        building_type,
        BuildingType::FletchersWorkshop
            | BuildingType::BlacksmithsWorkshop
            | BuildingType::PoleturnersWorkshop
            | BuildingType::ArmourersWorkshop
    )
}

/// Пары (старт, финиш), которые нужны циклам мастерских. Логика отбора та же, что в
/// build_worker_distances: держать в синхроне (это проверяет тест в simulator.rs).
pub(crate) fn worker_pairs(buildings: &[BuildingPlacement]) -> Vec<(u32, u32)> {
    let ids_of = |resource| {
        buildings
            .iter()
            .filter(|b| b.stockpile_resource == Some(resource))
            .map(|b| b.id)
            .collect::<Vec<_>>()
    };
    let wood = ids_of(StockpileResource::Wood);
    let iron = ids_of(StockpileResource::Iron);
    let armouries = buildings
        .iter()
        .filter(|b| b.building_type == BuildingType::Armoury)
        .map(|b| b.id)
        .collect::<Vec<_>>();

    let mut pairs = Vec::new();
    for workshop in buildings.iter().filter(|b| is_workshop(b.building_type)) {
        let stockpiles = match workshop.building_type {
            BuildingType::FletchersWorkshop | BuildingType::PoleturnersWorkshop => &wood,
            _ => &iron,
        };
        for stockpile in stockpiles {
            pairs.push((workshop.id, *stockpile));
            pairs.push((*stockpile, workshop.id));
        }
        for armoury in &armouries {
            pairs.push((workshop.id, *armoury));
            pairs.push((*armoury, workshop.id));
        }
    }
    for armoury in &armouries {
        for stockpile in wood.iter().chain(&iron) {
            pairs.push((*armoury, *stockpile));
        }
    }
    pairs
}