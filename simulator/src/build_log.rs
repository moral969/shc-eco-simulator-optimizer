//! Журнал последовательности строительства: по нему таймлайн под картой
//! заново проигрывает расстановку шаг за шагом.

use serde::{Deserialize, Serialize};

use crate::{BuildingType, StockpileResource};

/// Один шаг построения. Координаты те же, что у place_building / place_wall / remove_at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BuildStep {
    Building {
        building_type: BuildingType,
        x: i32,
        y: i32,
    },
    Wall {
        start_x: i32,
        start_y: i32,
        end_x: i32,
        end_y: i32,
    },
    /// Удаление того, что стоит в клетке (здание, клетка стены или, при whole_wall, вся стена).
    Remove {
        x: i32,
        y: i32,
        whole_wall: bool,
    },
    RemoveAllWalls,
    Stockpile {
        x: i32,
        y: i32,
        resource: StockpileResource,
    },
}

impl BuildStep {
    pub fn describe(&self) -> String {
        match self {
            Self::Building { building_type, x, y } => {
                format!("Placed {} at ({}, {})", building_type.display_name(), x, y)
            }
            Self::Wall {
                start_x,
                start_y,
                end_x,
                end_y,
            } => format!("Wall ({}, {}) -> ({}, {})", start_x, start_y, end_x, end_y),
            Self::Remove {
                x,
                y,
                whole_wall: true,
            } => format!("Removed whole wall at ({}, {})", x, y),
            Self::Remove { x, y, .. } => format!("Removed at ({}, {})", x, y),
            Self::RemoveAllWalls => "Removed all walls".to_string(),
            Self::Stockpile { x, y, resource } => format!(
                "Marked stockpile at ({}, {}) as {}",
                x,
                y,
                resource.display_name()
            ),
        }
    }
}