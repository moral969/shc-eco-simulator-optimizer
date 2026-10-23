# Empirical production model before the full-cycle replacement

Saved 2026-10-20. These are historical calibration values, not active settings.
The replacement keeps one production model in Rust; there is no legacy-mode switch.

| Product | Inputs | Old work ticks | Sell gold |
| --- | --- | ---: | ---: |
| Bow | 2 wood | 625 | 15 |
| Crossbow | 3 wood | 555 | 30 |
| Spear | 1 wood | 290 | 10 |
| Pike | 2 wood | 872 | 18 |
| Sword | 1 iron | 1090 | 30 |
| Mace | 1 iron | 910 | 30 |
| Metal armour | 1 iron | 585 | 30 |

Old weapon cycle: work ticks plus all route lengths multiplied by 24 ticks/tile.
Each input was carried individually. Fletcher normally returned from armoury to
workshop before collecting wood; its routing option skipped that return.
Other workshops used armoury -> input stock -> workshop -> armoury, repeating
workshop/input-stock trips for additional input units. Handling, deposit gates,
transfer states and animation-clock phase were not simulated.

Old weapon output: a synthetic ten-operation ring, initially
`[1,2,1,2,1,2,1,2,1,2]`. Each negative fear step replaced one of the remaining
ones with two. Average output was `1.5 + 0.1*abs(clamp(fear,-5,0))`.

Old bread-chain formulas:

```text
wheat output per field = 24 + 12*abs(clamp(fear,-5,0))/5
wheat cycle ticks = 6950 + 12*distance*(16+12)
mill worker cycle = 312 + 16*(flourStock->wheatStock + wheatStock->mill + mill->flourStock)
mill capacity/tick = min(3/workerCycle, 1/312)
bakery cycle = 1700 + 24*(granary->flourStock + flourStock->bakery + bakery->granary)
bread per flour = 8 + 4*abs(clamp(fear,-5,0))/5
```

The old bakery picked the granary with the shortest complete loop. The old wheat
model had no individual crops, field layout, growth pulse or worker state.
The three-farm calibration at distances `[16,4,8]` gave
`sum(24*3000/(6950+336*d)) = 21.9927147977` wheat per 3000 ticks.

The previous general snapshot is
`empirical_model_before_native_upgrade_2026-10-19.json`. The earlier weapon
backup `weapon_animation_ticks_before_dll_update_2026-10-14.json` is also retained.
