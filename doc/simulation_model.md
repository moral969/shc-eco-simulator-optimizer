# Simulation model

This document records the rules currently implemented by SHC Eco Simulator. It is a technical reference; the main README focuses on using the application.

## Canvas and placement

- Coordinates are signed integer cells and may be negative.
- A building is positioned by its bottom-left cell `(x, y)`.
- Occupied cells cannot overlap another blocking building cell or wall.
- The canvas expands after placement when necessary to retain a 50-cell margin between every placed object and each boundary.
- Canvas bounds are persisted in version 2 project files and restored exactly.
- A Wheat Farm reserves a 9x9 area. Its 3x3 top-left cabin blocks paths; the remaining field cells are traversable.
- A Wind Mill is 3x3 with a fixed bottom-center entry.
- Workshops, Armouries, Bakeries, and Granaries use 4x4 footprints.
- A Goods Yard creates four independent 2x2 Stockpiles in a 5x5 template. Removing one member removes the full grouped Goods Yard.

## Entry points and walls

Buildings store an optional entry point. For an `n x n` square, the normal bottom candidate is `(x + floor(n/2), y - 1)`; a 2x2 Stockpile begins at `(x, y - 1)`.

If that candidate is blocked, side candidates are checked in orientation order, followed by corners. Walls can therefore force a workshop or Bakery to use a different side. The offset rotates with the building orientation, so the gate retains the same relative second-cell rule on every side. If no valid candidate is reachable, the entry point is absent.

Pathfinding uses eight-direction movement through unoccupied cells. Distances are directional `(start building, finish building)` records; the reverse direction is stored separately.

## Building costs

| Building | Wood | Gold |
| --- | ---: | ---: |
| Goods Yard / Stockpile | 0 | 0 |
| Armoury | 5 | 0 |
| Fletchers Workshop | 20 | 100 |
| Blacksmiths Workshop | 20 | 200 |
| Poleturners Workshop | 10 | 100 |
| Armourers Workshop | 20 | 100 |
| Wheat Farm | 15 | 0 |
| Wind Mill | 20 | 0 |
| Bakery | 10 | 0 |
| Granary | 5 | 0 |

The setup report always converts required purchased build wood to gold when showing the total. It also preserves separate workshop and food-economy cost breakdowns.

## Goods prices

| Good | Sell gold | Buy gold |
| --- | ---: | ---: |
| Wood | 1 | 4 |
| Iron | 23 | 45 |
| Wheat | 8 | 23 |
| Flour | 10 | 32 |
| Bread and other food | 4 | 8 |
| Stone | 7 | — |

Stone is produced and sold directly by the additional-resource calculator.

## Movement

General movement speed is:

```text
cells per tick = 1 / (8 × (SB + 1))
```

- Weapon workshops and Bakeries use `SB = 2`, or 24 ticks/cell.
- Wheat farmers currently use the inspected `SB = 1`, `SP = 0` profile: 16 ticks/cell in both directions.
- Millers use 12 ticks/cell empty and 16 loaded. The old empirical empty-farmer=12 assumption is archived; its native switch remains unresolved.

See [unit_movement_speed.md](unit_movement_speed.md) for the detailed movement reference.

## Weapon production

Production uses the single [full-cycle model](full_cycle_model.md). Each operation
includes individual input trips, handling/transfer gates, native animation phases,
output selection, deposits and the next input/return path. Empirical whole-work
constants and the synthetic fear-output ring have been removed from runtime.
The previous values/formulas are saved in [the calibration archive](backups/empirical_cycles_before_replacement_2026-10-20.md).

| Weapon | Workshop | Wood | Iron | Normalized animation ticks | Sell gold |
| --- | --- | ---: | ---: | ---: | ---: |
| Bow | Fletcher | 2 | 0 | 581 | 15 |
| Crossbow | Fletcher | 3 | 0 | 509 | 30 |
| Spear | Poleturner | 1 | 0 | 246 | 10 |
| Pike | Poleturner | 2 | 0 | 543 | 18 |
| Sword | Blacksmith | 0 | 1 | 1044 | 30 |
| Mace | Blacksmith | 0 | 1 | 856 | 30 |
| Armor | Armourer | 0 | 1 | 512 | 30 |

The animation column excludes transport and handling. The reported full cycle is
measured over repeated complete batches; tooltips include mean and variation.
Pikes include their rotating between-input idle sequences; armourers include a
second 30-call local-buffer check. Output uses native per-worker fractional carry.

A Fletcher normally returns via its workshop before collecting the next wood.
**Optimize Fletcher routing** sends it directly from armoury to the input stack.
Other workshops take that successful direct input route. New armoury selection
uses ascending eligible IDs rather than the shortest complete loop.

### Iron supply

Iron mines produce only when the layout contains a Stockpile marked as Iron. Produced iron is used by workshops first. When **Buy Iron** is disabled, insufficient production scales iron workshop output down. When buying is enabled, only the deficit is bought at 45 gold. Any produced surplus is sold at 23 gold.

## Bread production

Wheat farms simulate 36 crop cells, both persistent layouts, the 151-update growth
pulse, cached farm task, farmer animations, internal paths, bundles and deposits.
Growth overlaps work and transport. There is no constant 6950-work-tick field
cycle or assumption of twelve identical cabin/stock round trips.

Mills schedule three workers against one processor. Grinding takes 272 aligned
updates and the native wheel/reset gate also controls the next claim: the
zero-walk three-worker fixture outputs one flour every 302 ticks. Walking and
queueing determine the capacity on the actual layout.

Bakeries simulate collection, transfer, native 1632-normalized-tick processing,
output selection and individual bread deposits. The negative departure timer
advances while walking; handling is not blindly added on top of travel. Granary
selection uses ascending IDs rather than the shortest loop.

Actual bread throughput is limited by wheat supply, mill capacity or bakery
capacity. Buy Wheat / Buy Flour fill the permitted deficit; input costs are
deducted from bread income. Reports retain existing surplus, purchase, bottleneck
and per-building output fields and add cycle timing statistics.

Capacity runs assume available inputs/storage, assigned workers and flat routes
without terrain or crowd delays. This is not a finite-inventory/hiring simulation.
Apple farms remain unsupported. See [the implementation and validation details](full_cycle_model.md).

## Population and additional economy

Additional-resource inputs are always active; population-specific inputs apply only when **Count population economy** is checked.

### Food

Normal consumption is `0.6 food/person/min`. The discrete food ratios are:

| Ratio | Food multiplier | Popularity |
| --- | ---: | ---: |
| No food | 0.0x | -8 |
| Half | 0.5x | -4 |
| Normal | 1.0x | 0 |
| Extra | 1.5x | +4 |
| Double | 2.0x | +8 |

Bread eaten by the population cannot be sold. The complete-economy calculation deducts consumed bread from layout sale income and sells only the remainder.

### Tax

A game month is 800 ticks. Tax gold per minute is:

```text
population × coefficient × (game speed × 60 / 800)
```

| Popularity | Coefficient | Popularity | Coefficient |
| ---: | ---: | ---: | ---: |
| +7 | -1.0 | -6 | 1.0 |
| +5 | -0.8 | -8 | 1.2 |
| +3 | -0.6 | -12 | 1.4 |
| +1 | 0.0 | -16 | 1.6 |
| -2 | 0.6 | -20 | 1.8 |
| -4 | 0.8 | -24 | 2.0 |

### Inns

- Build cost: 20 wood + 100 gold.
- Operation: 11.2 gold/min and one worker per Inn.
- Capacity: 30 population per Inn.
- Popularity: +8 at full coverage, +6 above 75%, +4 above 50%, +2 above 25%, otherwise 0.

### Stone and iron

| Resource building | Build wood | Workers | Base production at FF 0 |
| --- | ---: | ---: | ---: |
| Stone Quarry + ox | 25 | 4 | 18.6 stone/min |
| Iron Mine | 20 | 2 | 2.63 iron/min |

Production rises linearly to 1.33x at Fear Factor -5. Stone is valued at 7 gold/unit. Iron follows the workshop deficit/surplus rules above.

Total popularity is Tax + Food + Inn + Fear Factor. The UI treats zero or higher as acceptable and negative totals as a warning. Total gold combines workshop income, bread remaining after food, tax, stone sales, iron savings/surplus sales or purchases, and Inn beer expense.
