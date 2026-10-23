use crate::buildings::BuildingPlacement;

use super::{MapBounds, MapError};

#[derive(Debug, Clone)]
pub struct CellMap {
    bounds: MapBounds,
    cells: Vec<Option<u32>>,
    blocked: Vec<bool>,
    wall_counts: Vec<u32>,
}

impl CellMap {
    pub fn new(size: usize) -> Self {
        let bounds = MapBounds::square(size).expect("cell map size must fit in signed coordinates");
        Self::with_bounds(bounds).expect("square cell map bounds must be valid")
    }

    pub fn with_bounds(bounds: MapBounds) -> Option<Self> {
        if !bounds.is_valid() {
            return None;
        }
        let area = bounds.width().checked_mul(bounds.height())?;
        let mut cells = Vec::new();
        cells.try_reserve_exact(area).ok()?;
        cells.resize(area, None);
        let mut blocked = Vec::new();
        blocked.try_reserve_exact(area).ok()?;
        blocked.resize(area, false);
        let mut wall_counts = Vec::new();
        wall_counts.try_reserve_exact(area).ok()?;
        wall_counts.resize(area, 0);

        Some(Self {
            bounds,
            cells,
            blocked,
            wall_counts,
        })
    }

    pub fn bounds(&self) -> MapBounds {
        self.bounds
    }

    pub fn width(&self) -> usize {
        self.bounds.width()
    }

    pub fn height(&self) -> usize {
        self.bounds.height()
    }

    #[deprecated(note = "use width and height for rectangular maps")]
    pub fn size(&self) -> usize {
        self.width()
    }

    pub fn is_occupied(&self, x: i32, y: i32) -> bool {
        let Some(index) = self.index(x, y) else {
            return false;
        };
        self.cells[index].is_some() || self.wall_counts[index] > 0
    }

    pub fn is_in_bounds(&self, x: i32, y: i32) -> bool {
        self.bounds.contains(x, y)
    }

    pub fn is_blocked(&self, x: i32, y: i32) -> bool {
        self.index(x, y)
            .is_some_and(|index| self.blocked[index] || self.wall_counts[index] > 0)
    }

    pub fn can_place(&self, placement: &BuildingPlacement) -> Result<(), MapError> {
        self.can_place_cells(placement.occupied_cells())
    }

    pub fn place(&mut self, placement: &BuildingPlacement) -> Result<(), MapError> {
        self.can_place(placement)?;

        for (x, y) in placement.occupied_cells() {
            let idx = self.index(x, y).ok_or(MapError::OutOfBounds)?;
            self.cells[idx] = Some(placement.id);
        }
        for (x, y) in placement.blocking_cells() {
            let idx = self.index(x, y).ok_or(MapError::OutOfBounds)?;
            self.blocked[idx] = true;
        }

        Ok(())
    }

    pub fn can_place_cells(
        &self,
        cells: impl IntoIterator<Item = (i32, i32)>,
    ) -> Result<(), MapError> {
        for (x, y) in cells {
            if !self.is_in_bounds(x, y) {
                return Err(MapError::OutOfBounds);
            }
            if self.is_occupied(x, y) {
                return Err(MapError::Occupied);
            }
        }
        Ok(())
    }

    pub fn place_wall_cells(
        &mut self,
        cells: impl IntoIterator<Item = (i32, i32)>,
    ) -> Result<(), MapError> {
        let cells: Vec<(i32, i32)> = cells.into_iter().collect();
        for (x, y) in cells.iter().copied() {
            let index = self.index(x, y).ok_or(MapError::OutOfBounds)?;
            if self.cells[index].is_some() {
                return Err(MapError::Occupied);
            }
        }

        for (x, y) in cells {
            let idx = self.index(x, y).ok_or(MapError::OutOfBounds)?;
            self.wall_counts[idx] += 1;
        }

        Ok(())
    }

    pub fn clear_wall_cells(&mut self, cells: impl IntoIterator<Item = (i32, i32)>) {
        for (x, y) in cells {
            let Some(idx) = self.index(x, y) else {
                continue;
            };
            self.wall_counts[idx] = self.wall_counts[idx].saturating_sub(1);
        }
    }

    pub fn clear_cells(&mut self, cells: impl IntoIterator<Item = (i32, i32)>) {
        for (x, y) in cells {
            let Some(idx) = self.index(x, y) else {
                continue;
            };
            self.cells[idx] = None;
            self.blocked[idx] = false;
        }
    }

    pub(crate) fn index(&self, x: i32, y: i32) -> Option<usize> {
        if !self.is_in_bounds(x, y) {
            return None;
        }
        let local_x = usize::try_from(i64::from(x) - i64::from(self.bounds.min_x)).ok()?;
        let local_y = usize::try_from(i64::from(y) - i64::from(self.bounds.min_y)).ok()?;
        local_y.checked_mul(self.width())?.checked_add(local_x)
    }
}
