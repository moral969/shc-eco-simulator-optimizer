# Full production-cycle model

Implemented 2026-10-20. This replaces the empirical cycle formulas for every
currently placeable production building. There is one active calculation path.
The old values and formulas are preserved in
[the empirical archive](backups/empirical_cycles_before_replacement_2026-10-20.md).

## Supported production

- Fletcher: bows and crossbows, individual wood trips and optional direct routing.
- Poleturner: spears and pikes, entry/exit transfers and the pike's between-input idle animations.
- Blacksmith: swords and maces, direct hide/show handling.
- Armourer: metal armour, entry/exit transfers, local output collection and its second preparation gate.
- Wheat farm: 36 crops, two persistent layouts, growth pulses and twelve individual bundle deliveries.
- Mill: three millers, one processor, native slot-order claims, post-processing/reset and transport.
- Bakery: flour collection, entry/exit transfers, processing, granary selection and per-item bread deposits.

Stockpiles, armouries and granaries are destinations/storage, not independent
producers. Apple farms are not added. The separate count-based stone, iron,
population/tax/inn calculator is outside this building-cycle replacement.

## Clocks, output and measurement

`simulation/cycle_timing.rs` owns the shared clock/yield rules and the workshop,
mill and bakery cycles. Building animation advances at `elapsed >= interval`;
worker animation advances at `elapsed > delay`. Clocks retain phase across task
changes. An active building's restored run flag also preserves the one skipped
manager-clock visit after stopping.

The recipe contains inputs, product and value. It no longer contains a calibrated
whole-work duration. Native phase lengths are the active processing data:

| Product | Phase positions | Interval | Normalized processing ticks |
| --- | --- | ---: | ---: |
| Bow | 145, immediate final phase | 4 | 581 |
| Crossbow | 127, immediate final phase | 4 | 509 |
| Spear | 104, 19 | 2 | 246 |
| Pike | 80, 33, 33, 35 | 3 | 543 |
| Sword | 84, 61, 61, 55 | 4 | 1044 |
| Mace | 30, 74, 110 | 4 | 856 |
| Metal armour | 111, 17 | 4 | 512 |
| Bread | 392, 16 | 4 | 1632 |

These are **processing spans, not complete cycles**. Full cycles also include
successful input searches and arrivals, 30-call preparation, worker-specific
17-call transfers, 11-call destination searches, returns and deposits. A return
order issued by the next worker dispatch contributes its setup visit. The
five-call deposit timer runs during walking; the baker starts that timer at -10.

Pikes retain worker animation phase while returning with the first wood. The
between-input sequence rotates through 156, 54, 26 and 100 positions at delay 2.
Loaded walking can consume part of that sequence. Armourers also perform a
30-call local-buffer/output check after processing before leaving the building.

Yield uses a per-worker integer remainder. Ordinary productivity is
`100 + 10*abs(clamp(fear,-5,0))`; eligible weapon outputs add 50 percentage points
before quantization. Bakery input and output calls share that remainder. The old
handwritten ten-operation weapon ring is removed.

`CycleTiming` reports the first modelled complete batch, mean repeated interval,
minimum/maximum interval and mean output. Workshops/bakeries sample twenty repeat
intervals after startup; wheat samples ten field-output intervals; mills sample
46 intervals after the initial deliveries. Statistics are finite-window results,
not a guarantee of an infinite-horizon period. Workshop public totals round the
mean to a tick; tooltips include its unrounded mean and variation.

## Wheat and paths

`simulation/wheat_cycle.rs` implements crop and farmer states. It uses the native
151-update crop/task pulse, ordered target selection, preparation at index 8,
sow-on-arrival, harvesting at index 1, every-third-crop bundles, 144/52-position
idle sequences, stock search and individual deposits. Stock-to-crop continuation
is represented directly. Growth runs concurrently with worker tasks.

The constructor's ordered coordinates are translated from native Y-down to the
editor's Y-up coordinates. Each farm stores layout 0 or 1; placing farms toggles
the next layout. Layout and the next counter survive save/load and deletion.
Older project files infer alternating layouts in saved placement order.

Crop approaches use actual map routes and endpoint trimming. The shortest-route
tie rule prefers the target direction. Routes are cached within a farm run.
The farmer uses the inspected B=1/U=0 profile: 16 ticks/tile both loaded and empty.
The former empirical 12-tick empty farmer return is preserved in the archive;
its automatic native U transition has not been established.

## Mill reset and throughput

The mill processes only on every second building update. A fresh processing pass
uses 136 body calls (272 aligned updates). After the active worker releases the
slot, the 15-position wheel/reset sequence at RVA `0x2E1BE0` must finish before
phase zero allows another claim. With all three workers present and no walking,
this gives **302 ticks between repeated flour deliveries**, rather than treating
272 as a building-wide output interval. Carrier departure uses its own animation
index gate 20. Empty trips use 12 ticks/tile, loaded trips 16.

## Economy and interface integration

The bread report computes fully supplied worker capacities, then limits actual
chain throughput by wheat, flour and bakery supply. Purchase settings fill the
remaining permitted deficit. This is a capacity planner: finite inventories,
storage saturation and hiring are not advanced together as a live game world.

Bakery granary selection uses ascending building IDs rather than the shortest
complete route. New armoury selection uses the first eligible ID rather than
the fastest loop. The existing designated resource stacks remain the input
selection mechanism for the planner.

Cycle capacities are cached by layout/geometry and fear factor in `Simulator`.
The background backend precomputes them during recalculation/load. Changes to
game speed and resource purchase switches only recompute rate/accounting
reports. Geometry/resource/path changes invalidate the cache. Slint displays
results; it does not run worker state logic.

Worker input/handling states and production are supplied with available input
and output space. Routes remain a **flat, uncrowded map approximation**: native
terrain masks, height, crowd delay, exact path reconstruction/substeps and rest
interruptions are not represented. No running game, BepInEx or DLL is required
by the released app.

## Validation evidence

Native binary SHA-256:
`fbcb93195fc7efca9bdac5204852efdd76f9818f59a6711750d77c9cef2831e2`.

[Saved original-instruction probes](research/full_cycle_native_validation_2026-10-20.json)
record zero-walk and moving workshop/bakery fixtures, plus three native miller
dispatches. Their integration helpers are controlled stubs; processing and
worker instructions execute from the binary in Unicorn, not in a game process.

Regression anchors include:

- Crossbow's first zero-walk complete output at tick 562.
- Mean zero-walk cycles: spear 331, sword 1095, mace 907, metal armour 627 and bread 1729 ticks.
- With workplace/input/output points (100,100)/(102,100)/(97,100), flat 24-tick steps:
  crossbow 991, spear 565, sword 1327, mace 1139, metal armour 859 and bread 1953 ticks.
- Three millers: first flour at tick 296; repeated delivery interval 302 ticks.
- [Wheat reference trace](research/wheat_cycle_trace_2026-10-20.json): layout 0,
  four-tile southern stock route, first complete field output at tick 8184 and
  repeated output every 8154 ticks under the same synthetic walking assumptions.

Run `cargo test --workspace`, `cargo fmt --all -- --check` and
`cargo clippy --workspace --all-targets --all-features -- -D warnings`.
