# 5100-gold, six-minute workshop layout

Open [`2k8pt_5100_optimal_workshops.json`](2k8pt_5100_optimal_workshops.json) and enable **Show building entry points**. This version optimizes the gold remaining after six minutes, not workshop count or steady-state output alone.

## Result

- Setup budget: **5,100 gold exactly**
- Direct building gold: 3,200
- Bought build wood: 475 x 4 = 1,900 gold
- Workshops: 15 Crossbow Fletchers + 17 Spear Poleturners
- Support: 1 Goods Yard + 1 Armoury
- Workshop result: **1,801.49 gold/min** after buying production wood
- Six-minute sales after production inputs: 10,808.94 gold
- Six-minute result after recovering setup: **5,708.94 gold**
- Final walls: **0**

The older clustered geometry reaches 1,678.89 gold/min with its best six-minute mix, leaving 4,973.35 gold after setup. The new lane layout improves the six-minute result by **735.59 gold**.

## New layout idea

The Goods Yard and Armoury form the central hub at `(50,50)`. Workshops are arranged in horizontal lanes above and below it. Inner positions become Crossbow workshops because their extra 40-gold setup cost is repaid inside six minutes. Outer positions remain Spear workshops because travel makes the Crossbow upgrade fail that payback test.

**Optimize Fletcher routing must remain enabled.** Disabling it changes the route and invalidates this result.

## Building coordinates

`C` means Crossbow Fletcher and `P` means Spear Poleturner. Each item is written as `type: bottom-left cell -> required entry cell`.

| Y | Workshops |
| ---: | --- |
| 36 | P: `(42,36) -> (46,40)`, P: `(46,36) -> (49,40)`, P: `(51,36) -> (50,40)`, P: `(55,36) -> (54,40)` |
| 41 | P: `(38,41) -> (42,45)`, C: `(42,41) -> (46,45)`, C: `(46,41) -> (49,45)`, C: `(51,41) -> (50,45)`, C: `(55,41) -> (54,45)`, P: `(59,41) -> (58,45)` |
| 46 | P: `(34,46) -> (38,50)`, P: `(38,46) -> (42,50)`, C: `(42,46) -> (46,50)`, C: `(46,46) -> (50,50)`, Armoury: `(51,46) -> (50,50)`, C: `(55,46) -> (54,50)`, P: `(59,46) -> (58,50)`, P: `(63,46) -> (62,50)` |
| Core | Stockpiles: `(48,50)`, Wood: `(51,50) -> (50,50)`, `(48,53)`, `(51,53)` |
| 51 | P: `(34,51) -> (38,50)`, P: `(38,51) -> (42,50)`, C: `(42,51) -> (46,50)`, C: `(55,51) -> (54,50)`, C: `(59,51) -> (58,50)`, P: `(63,51) -> (62,50)` |
| 56 | P: `(34,56) -> (38,55)`, P: `(38,56) -> (42,55)`, C: `(42,56) -> (45,55)`, C: `(46,56) -> (49,55)`, C: `(51,56) -> (50,55)`, C: `(55,56) -> (54,55)`, C: `(59,56) -> (58,55)` |
| 61 | P: `(46,61) -> (49,60)`, P: `(51,61) -> (50,60)` |

## Repeatable gate setup

The saved project contains no walls. Use the map's entry labels as a blueprint and set one workshop gate at a time:

1. Place the surrounding/touching workshops that already block unwanted sides.
2. Before placing the target workshop, use short temporary wall segments to block the remaining earlier gate candidates while leaving the listed entry cell open.
3. Place the workshop and confirm its green entry coordinate.
4. Remove the temporary wall immediately; the entry remains fixed.

For an ordinary side rotation, one wall along the opposite side is enough. For a listed shared corner, extend that temporary wall around the still-open side until the corner is the first available entry. The same small wall jig can be removed and reused for the next workshop, so the completed economy never contains a wall maze.

Build the center lanes first and work outward. If a gate differs from the table, remove only that workshop, repeat the wall jig, and rebuild it; the other saved gates do not need to change.
