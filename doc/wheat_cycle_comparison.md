# Wheat farm full cycle: DLL calculation versus empirical simulator

**Implementation update, 2026-10-20:** the app now uses one [full-cycle model](full_cycle_model.md). This document records the earlier research/proposal/comparison; descriptions of the then-current empirical runtime are historical. The [old calibration](backups/empirical_cycles_before_replacement_2026-10-20.md) is archived outside active code.

Research date: **2026-10-20**. The simulator's Rust formulas and executable are unchanged.

The farm produces **24 wheat from a clean 36-cell harvest at productivity 100**. Its elapsed cycle is the result of crop growth, worker animation, task checks and actual routes. There is no single verified whole-cycle DLL constant to substitute for the simulator's empirical 6,950.

The new calculation executes the original DLL worker and farm instructions offline, with synthetic flat-ground walking. This is stronger evidence for the state/timer calculation than adding animation durations, but **the resulting full-cycle numbers remain conditional estimates until native movement and live traces are matched**.

## What the empirical model currently calculates

Source: [`simulator/src/simulation/bread.rs`](../simulator/src/simulation/bread.rs).

```text
d = shortest 8-neighbor distance between farm and wheat-stock access points
T_empirical(d) = 6950 + 12*d*(16 + 12)
               = 6950 + 336*d
output at fear 0 = 24 wheat
rate per 3000 ticks = 24*3000 / T_empirical(d)
```

The model assumes twelve identical loaded/empty round trips, using 16 ticks/tile loaded and 12 empty. It does not instantiate crops or a farmer. The distance search is in `simulation/pathfinding.rs`; diagonal and cardinal steps both count as one tile.

| Access distance d | Empirical full cycle, ticks | Seconds if actual TPS = 50 | Wheat per 3000 ticks |
| ---: | ---: | ---: | ---: |
| 0 | 6,950 | 139.00 | 10.360 |
| 4 | 8,294 | 165.88 | 8.681 |
| 8 | 9,638 | 192.76 | 7.470 |
| 16 | 12,326 | 246.52 | 5.841 |

The existing calibration test uses **distances [16, 4, 8]**, producing **21.992715 wheat per 3000 ticks**, approximately 22. It stores these scalar distances and a target rate, not the original in-game map, field layouts or delivery timestamps. The comparison below therefore cannot be described as a reproduction of that original live experiment.

The preserved [empirical backup](backups/empirical_model_before_native_upgrade_2026-10-19.json) still matches the current `bread.rs` SHA-256: `b5c290962d967926f441c03e069869d35f1c21d3bf6cbbc981f51ac3257b714e`.

## DLL rules used in the calculation

All addresses are RVAs in the SHA-256-pinned `CrusaderDE.dll` listed below.

| Component | Native evidence | Effect on elapsed time |
| --- | --- | --- |
| Crop geometry | Constructor `0x776F0`, table `0x2D37C0` | 36 ordered crop positions; two alternating layouts |
| Farm update | `0xA36D0` | Crop/task pulse after counter becomes >150: every 151 updates |
| Crop growth and cached task | `0xC8840` | State 2 reaches ripe state 11 after nine pulses, 1,209–1,359 ticks after sowing |
| Target selection | `0xBBE00` | First matching position in the farm's stored list, not nearest crop |
| Worker | `0x133DD0` | Preparation, sowing, harvesting, bundle collection and delivery state machine |
| Preparation sequence | `0x332178` | 16 positions at a normalized three-tick interval; prepared-state write at index 8 |
| Sowing | Worker state 5 | Writes crop state 2 on arrival; no additional 48-tick work animation |
| Harvest sequence | `0x3322A0` | 16 positions; crop changed at index 1; every third harvested crop makes one bundle |
| Idle/task-check sequences | `0x3321A0` / `0x332250` | **144 / 52 positions**, nominally **432 / 156 ticks** with delay 2 |
| Animation clock | Original block `0x183CC1..0x183D68` | Advances when current clock > last clock + delay; clocks retain phase across worker states |
| Yield | `0x18D940(unit, 2, 0)` | At productivity 100, one bundle gives two wheat; no mode bonus for flag 0 |
| Stock search | Worker state 9 | 11 calls from timer zero when immediately successful |
| Deposit | Worker state 10 | Timer gate of five calls per wheat; timer also runs while walking |

The idle sequences explain part of the timing which animation-only sums miss. State 1 checks for work when its sequence finishes. The branch uses the field currently named `r_TicksSinceRestPulse` at unit +0x37C: zero selects the 144-position sequence; nonzero selects the 52-position sequence. Preparation writes one to this field; sowing clears it. Do not assume the worker rechecks the cached farm task every tick merely because crop ripeness has changed.

Task priority is: four or more bundles -> collect; otherwise more than eleven ripe crops, or ripe crops plus cleared state-120 cells -> harvest; otherwise any bundle -> collect; otherwise preparation/clearing -> sowing -> waiting. The task is cached by the farm pulse. Consequently, harvest and delivery interleave; a field season is not twelve deliveries appended after all harvesting finishes.

After a delivery, the worker can route directly from stock to a harvest/bundle target. It does not always return to the cabin. Growth proceeds while the farmer prepares other cells, walks, harvests and delivers. **Adding 1,359 to all work and walking would double-count overlapping time.**

## Controlled fixture and its limits

The new [replay script](research/wheat_native_replay.py) runs the original farm, crop/task selector, worker, yield, return helper, rest-gate check and animation-clock instructions in Unicorn. It never opens the running game or loads the DLL as executable native code.

Conditions:

- One assigned worker, already at access point **(101,103)**; cabin begins at **(100,100)** and blocks its 3×3 area. No hiring or campfire journey.
- All 36 crops initially unprepared. Farm growth counter initially zero unless the scenario states otherwise. Worker clocks initially zero, animation delay 2.
- Productivity 100, native mode M=1, rest gate inactive, sufficient storage capacity, successful stock selection and no interruptions.
- Stock access is **(101,103+d)** in the main comparison. Only the cabin blocks the synthetic map. Distance zero co-locates access points as an abstract control, not a guaranteed buildable layout.
- **Synthetic movement:** shortest 8-neighbor routes, fixed ticks per step, no crowd delay, slope or substep phase. Equal-length routes prefer points closer to the target. Endpoint trimming follows the inspected subtract-one-step rule. Arrival, storage validation, facing and graphics integration are isolated stubs; production counter recording is omitted.
- One building dispatch, then synthetic movement, original unit animation clock and one worker dispatch per simulation tick.
- Each full comparison runs to **144 deposited wheat**. The five 24-wheat intervals after the first 24 give the reported repeat mean. Variable intervals are a finite sample, not a proven infinite-horizon period.

Two walking profiles are compared because the speed provenance is still unresolved:

1. **Retained U=0:** 16 ticks/tile in both directions, consistent with the inspected farmer callbacks retaining U=0 and B=1.
2. **Empirical switch hypothesis:** 12 empty / 16 loaded, extending the simulator's empirical walking coefficients to the individual routes. The callback probes do not establish this as an automatic native U transition.

Thus, the state/timer logic is original code; path choice, walking and these integration conditions are assumptions. A difference from the empirical formula is not by itself proof that an in-game observation was wrong.

## Detailed full cycle: layout 0, distance 4, retained U=0

The [detailed trace](research/wheat_cycle_trace_2026-10-20.json) repeats the same **8,154-tick** interval. The larger comparison confirms five consecutive intervals of that length.

### First season from an assigned worker

| Event | Simulation tick |
| --- | ---: |
| First work selection after initial idle | 432 |
| First preparation animation begins | 464 |
| First crop becomes prepared | 486 |
| Last of 36 crops becomes prepared | 2,511 |
| Last preparation animation ends | 2,535 |
| First crop sown | 2,804 |
| Last crop sown | 3,396 |
| First crop reaches ripe state 11 | 4,077 |
| Twelfth crop reaches ripe state; pulse can select harvesting | 4,228 |
| Worker selects harvesting after its idle sequence | 4,341 |
| First harvest animation begins | 4,373 |
| First crop marked harvested | 4,374 |
| First bundle collected | 5,179 |
| First wheat deposited | 5,270 |
| Second wheat from that bundle deposited | 5,275 |
| Final crop of the first field marked harvested | 7,674 |
| All 24 wheat from the first field deposited | **8,184** |
| Next field's first preparation animation begins | 8,711 |

The first season's 24 wheat are deposited in batches throughout ticks 5,270–8,184. They do not all appear at the end. At a measured 50 TPS, the first wheat arrives at 105.40 seconds and the first complete 24 at 163.68 seconds.

### Repeated full-cycle accounting

Between the next two first-preparation events, ticks **8,711 -> 16,865**, the native worker-state visits partition the full **8,154 ticks** as follows:

| Worker activity | State | Elapsed ticks |
| --- | ---: | ---: |
| Idle animation and task checks | 1 | 1,450 |
| Return/home handling and walking | 2 | 227 |
| Approach preparation targets | 3 | 412 |
| Prepare 36 cells | 4 | 1,691 |
| Walk and sow 36 cells | 5 | 624 |
| Approach harvest targets | 6 | 587 |
| Harvest 36 cells | 7 | 1,689 |
| Approach and collect 12 bundles | 8 | 498 |
| Twelve stock searches, 11 calls each | 9 | 132 |
| Carry and deposit 24 wheat | 10 | 844 |
| **Total** | | **8,154** |

This is a sum of mutually exclusive worker states, so it does not add crop growth a second time. Preparation plus harvest use 3,380 elapsed state visits in this trace. The 3,456 figure (`36*2*16*3`) is a normalized animation estimate; retained clock phase and state boundaries account for the difference. Neither should be substituted alone for the full cycle.

The twelve loaded paths total 49 tiles in this repeated interval. State 10 therefore uses `49*16 + 12*5 = 844` ticks here: walking absorbs the first deposit gate; each bundle's second wheat adds five ticks. The three return/home paths total 14 tiles; field-to-field and stock-to-field routes are additional individual paths.

Cycle comparison for this fixture:

```text
Empirical:       6950 + 336*4 = 8294 ticks; 8.680974 wheat / 3000 ticks
DLL-state replay:               8154 ticks; 8.830022 wheat / 3000 ticks
Difference:                    -140 ticks (-1.69% cycle time)
```

## Results across distances and field layouts

These are the southern-stock fixtures defined above, not measurements in the running game. Decimal tick values are averages of integer intervals.

### Retained U=0: 16 ticks per tile throughout

| d | Empirical ticks | Layout 0 mean ticks | Layout 1 mean ticks | Layout 0 wheat / 3000 ticks | Layout 1 wheat / 3000 ticks |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 0 | 6,950 | 8,439.8 | 9,269.4 | 8.531 | 7.767 |
| 4 | 8,294 | 8,154.0 | 9,211.2 | 8.830 | 7.817 |
| 8 | 9,638 | 9,271.8 | 9,543.0 | 7.765 | 7.545 |
| 16 | 12,326 | 12,230.4 | 12,286.8 | 5.887 | 5.860 |

### Empirical speed hypothesis: empty 12, loaded 16

| d | Empirical ticks | Layout 0 mean ticks | Layout 1 mean ticks | Layout 0 wheat / 3000 ticks | Layout 1 wheat / 3000 ticks |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 0 | 6,950 | 8,417.0 | 8,491.4 | 8.554 | 8.479 |
| 4 | 8,294 | 8,593.2 | 8,695.6 | 8.379 | 8.280 |
| 8 | 9,638 | 8,840.2 | 9,508.2 | 8.145 | 7.572 |
| 16 | 12,326 | 11,628.0 | 11,246.2 | 6.192 | 6.402 |

For three independent benchmark farms at distances [16,4,8], summing the sampled rates gives **21.22–22.48** wheat/3000 ticks with retained U=0 across the two all-layout benchmarks, or **22.25–22.72** with the empirical speed hypothesis. The old model gives **21.99**. These are comparisons of model scale; they do not recreate a shared three-farm map, collisions, actual layout alternation or the original empirical experiment.

Changing walking speed can also change which 151-tick crop pulse or idle-animation boundary the farmer catches. A faster walk does not imply that every finite sampled whole cycle will be shorter.

## Control calculations: why distance alone is insufficient

| Changed condition, layout 0 / d=4 / U=0 unless stated | Mean repeat ticks per 24 wheat |
| --- | ---: |
| Main fixture, southern stock, target-oriented route ties | 8,154.0 |
| Initial crop pulse counter 75 | 8,601.8 |
| Initial crop pulse counter 150 | 8,154.0 |
| Western stock, same cabin-to-stock distance 4 | 9,994.8 |
| Different equal-length route tie rule | 8,896.8 |
| **All walking instantaneous, including crop-to-crop** | **6,942.0** |

The instantaneous-walking control produces five consecutive 6,942-tick repeat intervals. This is **8 ticks / 0.115% below the empirical 6,950 work constant**. It demonstrates that roughly this duration can emerge from the native work/wait/state sequence. It does not prove a 6,950 constant exists in the DLL or that a normal placed farm has no internal walking.

In particular, **d=0 is different from zero walking**: d=0 only co-locates cabin and stock access; crops still occupy different cells. The scalar-distance formula cannot encode the stock's side relative to the field. The route-tie control also demonstrates that the synthetic path approximation is material, rather than an insignificant implementation detail.

## What this means for upgrading the simulator

Keep **6,950 + 336*d** as the empirical profile. The current calibration has the correct output scale and remains useful as a regression reference.

A DLL-based profile should execute 36 crop states, cached task/pulse timing, both idle sequences, worker animation phase and twelve individual bundle deliveries using worker-to-target paths. It should report startup separately from repeated delivered output. Replacing 6,950 with 3,456, or with 3,456+1,359, would discard substantial verified behavior.

Before treating any calculated whole-cycle number as an exact in-game prediction, the remaining steps are:

1. Resolve/record wheat farmer U changes during complete live empty and loaded routes; preserve the empirical 12/16 profile until then.
2. Match native path reconstruction, endpoint trimming, movement substeps and arrival timing, including equal-length route choices.
3. Capture the original calibration geometry, field layout, farm pulse counter, worker animation clocks and actual stock access point. Distances alone cannot reconstruct them.
4. Compare first delivery, all 24 deposit ticks and several subsequent 24-wheat intervals. The native worker's averaged production-counter field is not a substitute for those global tick timestamps.

## Evidence and reproduction

Binary SHA-256: `fbcb93195fc7efca9bdac5204852efdd76f9818f59a6711750d77c9cef2831e2`.

Installed binary used by the existing static harness: `C:\Program Files (x86)\Steam\steamapps\common\Stronghold Crusader Definitive Edition\Stronghold Crusader Definitive Edition_Data\Plugins\x86_64\CrusaderDE.dll`.

Native decompilation/assembly and replay dependency: `C:\repositories\shc-dashboard\artifacts\production-static`. Worker source is `decompiled/unit_FarmerWheatUpdate.c`; crop priority is `decompiled/farm_periodic_update.c`; animation-clock assembly is `decompiled/unit_manager_update.asm`. Original instructions are authoritative over inferred Ghidra types/names.

- [21-scenario calculated comparison](research/wheat_cycle_comparison_2026-10-20.json), including all deposit ticks, interval spans, stage totals and explicit fixture assumptions.
- [Detailed three-season trace](research/wheat_cycle_trace_2026-10-20.json), including individual crop writes, state changes and routes.
- [Replay source](research/wheat_native_replay.py). Uses the Dashboard harness's bundled Unicorn/PE libraries and verifies the pinned DLL hash. `--artifacts` can override the harness directory.
- [Earlier full production specification](native_production_cycles.md) and [implementation plan](dll_upgrade_plan.md).

```powershell
& 'C:\Program Files\Python39\python.exe' .\doc\research\wheat_native_replay.py
& 'C:\Program Files\Python39\python.exe' .\doc\research\wheat_native_replay.py --quick --trace --output .\doc\research\wheat_cycle_trace_2026-10-20.json
cargo test -p simulator simulation::bread::tests
```

Verification completed: 21 offline scenarios; detailed trace checked for 108 prepared/sown/harvested crops, 36 bundles and 72 deposited wheat; repeated reference intervals 8,154 ticks; four existing bread-model tests passed. No simulator Rust behavior, game files or live game memory were changed.
