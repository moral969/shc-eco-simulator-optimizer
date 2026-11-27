//! Быстрые показатели над картой: каталог, выбор и порядок.
//! Значения считает app.rs из уже готовых отчётов, ядро не затрагивается.

/// Сколько показателей влезает в полосу над картой.
pub const MAX_QUICK_STATS: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickStatId {
    TotalGold,
    ArmouryGold,
    GranaryGold,
    Wheat,
    Flour,
    Bread,
    Weapons,
    IronUse,
    WoodUse,
    Workers,
    FoodEaten,
    Popularity,
}

impl QuickStatId {
    pub const ALL: [Self; 12] = [
        Self::TotalGold,
        Self::ArmouryGold,
        Self::GranaryGold,
        Self::Wheat,
        Self::Flour,
        Self::Bread,
        Self::Weapons,
        Self::IronUse,
        Self::WoodUse,
        Self::Workers,
        Self::FoodEaten,
        Self::Popularity,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::TotalGold => "total_gold",
            Self::ArmouryGold => "armoury_gold",
            Self::GranaryGold => "granary_gold",
            Self::Wheat => "wheat",
            Self::Flour => "flour",
            Self::Bread => "bread",
            Self::Weapons => "weapons",
            Self::IronUse => "iron_use",
            Self::WoodUse => "wood_use",
            Self::Workers => "workers",
            Self::FoodEaten => "food_eaten",
            Self::Popularity => "popularity",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|stat| stat.id() == value)
    }

    pub fn group(self) -> &'static str {
        match self {
            Self::TotalGold | Self::ArmouryGold | Self::GranaryGold => "Economy",
            Self::Wheat | Self::Flour | Self::Bread => "Bread",
            Self::Weapons => "Weapons",
            Self::IronUse | Self::WoodUse => "Resources",
            Self::Workers | Self::FoodEaten | Self::Popularity => "Population",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::TotalGold => "Total economy",
            Self::ArmouryGold => "Armouries",
            Self::GranaryGold => "Granaries",
            Self::Wheat => "Wheat",
            Self::Flour => "Flour",
            Self::Bread => "Bread",
            Self::Weapons => "Weapons",
            Self::IronUse => "Iron use",
            Self::WoodUse => "Wood use",
            Self::Workers => "Workers",
            Self::FoodEaten => "Food eaten",
            Self::Popularity => "Popularity",
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Self::TotalGold | Self::ArmouryGold | Self::GranaryGold => "gold / min",
            Self::Workers => "total",
            Self::Popularity => "total",
            _ => "/ min",
        }
    }
}

/// Какие показатели включены и в каком порядке они стоят в полосе.
#[derive(Debug, Clone)]
pub struct QuickStatSelection {
    enabled: Vec<QuickStatId>,
}

impl Default for QuickStatSelection {
    fn default() -> Self {
        Self {
            enabled: vec![
                QuickStatId::ArmouryGold,
                QuickStatId::GranaryGold,
                QuickStatId::TotalGold,
            ],
        }
    }
}

impl QuickStatSelection {
    pub fn enabled(&self) -> &[QuickStatId] {
        &self.enabled
    }

    /// Все показатели для списка Customize: сначала включённые (в порядке показа), потом остальные.
    pub fn ordered_all(&self) -> Vec<(QuickStatId, bool)> {
        let mut result: Vec<_> = self.enabled.iter().map(|stat| (*stat, true)).collect();
        result.extend(
            QuickStatId::ALL
                .into_iter()
                .filter(|stat| !self.enabled.contains(stat))
                .map(|stat| (stat, false)),
        );
        result
    }

    pub fn ids(&self) -> Vec<&'static str> {
        self.enabled.iter().map(|stat| stat.id()).collect()
    }

    pub fn set_from_ids(&mut self, ids: &[String]) {
        let mut enabled = Vec::new();
        for stat in ids.iter().filter_map(|id| QuickStatId::from_id(id)) {
            if !enabled.contains(&stat) {
                enabled.push(stat);
            }
        }
        enabled.truncate(MAX_QUICK_STATS);
        self.enabled = enabled;
    }

    /// Включает/выключает показатель. false — полоса заполнена, включить нельзя.
    pub fn toggle(&mut self, id: &str) -> bool {
        let Some(stat) = QuickStatId::from_id(id) else {
            return false;
        };
        if let Some(index) = self.enabled.iter().position(|s| *s == stat) {
            self.enabled.remove(index);
            return true;
        }
        if self.enabled.len() >= MAX_QUICK_STATS {
            return false;
        }
        self.enabled.push(stat);
        true
    }

    /// Сдвигает включённый показатель на delta позиций (-1 влево, +1 вправо).
    pub fn move_by(&mut self, id: &str, delta: i32) {
        let Some(stat) = QuickStatId::from_id(id) else {
            return;
        };
        let Some(index) = self.enabled.iter().position(|s| *s == stat) else {
            return;
        };
        let target = (index as i32 + delta).clamp(0, self.enabled.len() as i32 - 1) as usize;
        let item = self.enabled.remove(index);
        self.enabled.insert(target, item);
    }
}