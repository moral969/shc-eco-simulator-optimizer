# Native production cycles: farms, mill, bakery and workshops

**Implementation update, 2026-10-20:** the app now uses one [full-cycle model](full_cycle_model.md). This document records the earlier research/proposal/comparison; descriptions of the then-current empirical runtime are historical. The [old calibration](backups/empirical_cycles_before_replacement_2026-10-20.md) is archived outside active code.

Research and current-source comparison: **2026-10-19**. This is a specification for an upgrade, not a change to the current simulation.

The DLL has interacting worker, building, resource and movement state machines. A single universal “work time” cannot describe a complete farm or workshop cycle. This report gives the individual rules and timings, including branches which overlap or skip stages. The implementation sequence is in [dll_upgrade_plan.md](dll_upgrade_plan.md).

## Evidence, clock units and confidence

Installed binary: `C:\Program Files (x86)\Steam\steamapps\common\Stronghold Crusader Definitive Edition\Stronghold Crusader Definitive Edition_Data\Plugins\x86_64\CrusaderDE.dll`.

SHA-256: `fbcb93195fc7efca9bdac5204852efdd76f9818f59a6711750d77c9cef2831e2`. Function addresses below are RVAs; field offsets refer to the start of the appropriate record.

Evidence consists of native assembly, Ghidra reconstructions, Script Extender field mappings, and offline Unicorn execution of original instructions against controlled fixtures. No live process, injection or modified game DLL was used. [Saved native evidence](research/native_production_evidence_2026-10-19.json) contains the new boundary tests and references to earlier movement, apple, farm-construction and fletcher replays.

Timing labels used here:

- **Normalized animation**: sequence length multiplied by its effective index interval, with the documented phase transitions and aligned initial clocks. This excludes walking, handling, shortages and rest.
- **State gate**: number of eligible worker calls from the stated initial timer, assuming successful checks. An ordinary active worker is dispatched during simulation updates, but pause/rest/context gating can change eligibility.
- **Elapsed cycle**: global simulation ticks between explicit events such as two completed deliveries. This must be measured by the simulator, separately from native worker production-counter visits.
- **Open integration detail**: not sufficiently established to prescribe one exact outcome for every map/state. These are listed explicitly below.

All durations are simulation ticks/eligible updates. To convert a measured elapsed result, use `seconds = elapsedTicks / measuredTPS`. The current app treats `game_speed_ticks_per_second` as TPS. A displayed in-game speed value should be calibrated against actual simulation ticks before interpreting real-time minutes.

The working cycles below begin with a live worker assigned to the building. Peasant spawning, hiring and the worker's first journey from the campfire are additional startup processes; they are not included in a craft or harvesting animation duration.

Native mode **M** means the value at RVA `0x8574B90`. Its nonzero branch changes workshop sequences, some speeds and selected yield calculations. Its exact correspondence to all game menu modes remains unresolved; do not silently rename it to a particular menu option. Most tables below use **M != 0** and give M=0 alternatives where relevant.

## Common movement and carrying

The relevant worker fields are:

| Field | Record offset | Use |
| --- | ---: | --- |
| `r_CurrentSpeed` | `0x348` / 840 | Base slowdown B |
| `r_CurrentSpeed2` | `0x346` / 838 | Actual slowdown SB, recalculated for terrain |
| `r_SpeedBonus` | `0x2BA` / 698 | Speed-up U; native arithmetic can treat it as signed |
| `r_PathPlanRelated1` | `0xF0` / 240 | Dynamic crowd delay D |
| `r_HeightElevation` | `0xB6` / 182 | Current interpolated height |
| `r_CurrentPositionTileId` | `0xD0` / 208 | Current occupancy tile |

The ordinary movement rule is:

```cpp
++movementClock;
if (movementClock - lastMovement > SB + U + D) {
    lastMovement = movementClock;
    D = max(D - 2, 0);
    attempt_substeps(U + 1);
}
```

There are eight substeps per tile. With constant SB/U, D=0 and ordinary uninterrupted walking:

```text
average ticks/tile = 8 * (SB + U + 1) / (U + 1)
```

Preserve integer clocks and substeps for individual trips. Diagonal walking does not use a sqrt(2) time multiplier. Occupancy transfers partway through the step; it is not identical to reaching the next tile centre.

| Worker, M != 0 | Empty ordinary travel | Loaded ordinary travel | Strength of finding |
| --- | --- | --- | --- |
| Apple/wheat/hops/dairy farmer | B=1; **16** ticks/tile if U=0 | Same B=1; **16** if U=0 | Tested callbacks retain U across these tasks; no generic load switch established |
| Miller | B=1, **U=1: 12** ticks/tile in states 2/3 | B=1, **U=0: 16** in states 4/8 | Explicit task writes, including pickup boundary |
| Baker | B=2; **24** if U=0 | Same; **24** if U=0 | U retained in tested travel states |
| Fletcher, poleturner, blacksmith, armourer, tanner | B=2; **24** if U=0 | Same; **24** if U=0 | M branch changes base; U retained in tested travel states |
| Brewer | B=1; **16** if U=0 | Same; **16** if U=0 | M branch changes base; U retained |
| Woodcutter | B=2; **24** if U=0 | Same tested B/U | Tree/cutting states are separate |

**There is no common “carrying goods makes every worker slower” rule.** The miller explicitly slows from 12 to 16 after pickup. For farmers, the callbacks retain a seeded U=1 as well as U=0; U=1/B=1 would give 12 in either tested travel task. The current empirical empty-farmer=12/loaded=16 rule is therefore not confirmed as a universal DLL transition. Native spawn `0x17FEF0` and stat initialization `0x19A240` inspected here do not establish all U initialization/reassignment paths. Initial U must remain an explicit state/validation item, rather than silently replacing the user's observation.

Other effects are independent of cargo:

- Ordinary ground edges require a height difference of at most **16** between adjacent cells, plus property/profile checks. Walls, stairs and special transitions have more rules.
- Uphill slowdown compares occupied tile height with current unit height. Gaps 1 / 2–3 / 4–7 / 8–11 / 12–16 assign B / B+1 / B+3 / B+4 / B+5 and raise unit height by 1 / 2 / 3 / 4 / 4. Recalculate after movement batches; do not apply the initial gap's penalty to the whole tile.
- Downhill gaps 4–7 / 8–11 / 12–16 lower height by 2 / 3 / 4 and assign B. Gaps 1–3 lower height by one while **preserving previous SB** before other terrain adjustments. Level ground restores the normal base/formation calculation.
- Ford adds +2 SB. Swamp/moat has a buildup-dependent +3/+4/+6 penalty; lingering buildup can add +3 after leaving. These are stateful effects.
- Shared occupancy can assign D again. Units can queue even on geometrically valid paths. Do not represent this by an invented constant percentage slowdown.

Detailed native movement and edge rules: [movement](../../shc-dashboard/SHCDE_UNIT_MOVEMENT.md), [pathfinding](../../shc-dashboard/SHCDE_UNIT_PATHFINDING.md).

## Shared searches, storage and access points

Input and output searches do not generally optimize walking distance:

| Operation | Native selection |
| --- | --- |
| Fetch wood/iron/flour/wheat/hops input, `0xB9280` | Scan building slots **descending**; first alive, correct-owner matching input store with sufficient goods |
| Output to stockpile, `0xBF9F0` | Scan ascending for an existing nonzero stack of the good below its cap; then scan ascending for an eligible empty stack |
| Output food to granary, `0xBF9F0` | Scan ascending; own granary with food total below 250 and accepted access check |
| New armoury selection, `0xB9AA0` | Ascending IDs, owner/alive/access checks and capacity for the whole carried load; cap is 50 |
| Existing armoury ID supplied | Validate that specific target; not necessarily a new scan |

Separate selection from route acceptance, arrival, global-ID validation and transfer. A chosen destination can become full/invalid before arrival. Preserve retry timers and retained targets. Artificially selecting the shortest complete bakery loop changes stock behavior.

Building entrances are cached and retained while valid. The native validator checks local conditions and connection to the player's keep-door region; a six-tile “can escape this pocket” flood fill is not equivalent. Normal regeneration scans the building perimeter with fixed ordering and fallback passes. Farms use the **3×3 cabin**, not the whole field square. Wall-facing workshop refreshes are staggered by `((tick + buildingGlobalId) & 255) == 0`. See [access-point research](../../shc-dashboard/SHCDE_BUILDING_ACCESS_POINTS.md).

## Apple orchard: full working cycle

Building type 32; worker type 13. Worker callback `0x135E30`, selector `0x108040`, tree updater `0x106A70`.

### Layout and tree availability

An orchard is one 3×3 cabin in a 10×10 farm area plus eight independent type-15 vegetation objects. Relative to the native cabin beginning coordinate, tree centres are `(5,1),(9,1),(1,5),(5,5),(9,5),(1,9),(5,9),(9,9)`. Initial world coordinates are `8*tile + 3`. Each tree has independent growth state/progress. New-tree progress receives a small initial value from tile variation, so a construction batch need not be perfectly synchronized.

| Tree growth stage | Duration | Harvest selection |
| ---: | ---: | --- |
| 0 | 500 | No |
| 1 | 300 | No |
| 2 | 200 | No |
| 3 | **1,000** | Yes |
| 4 | 40 | No |
| 5 | 40, then stage 0 | No |

The nominal period is **2,080 ticks**; a zero-progress stage-0 tree reaches ripeness after 1,000. Harvesting does not subtract a finite apple count or reset this timer. Multiple farmers can use the same ripe tree.

### Search and approach

Search chooses the nearest alive type-15 tree at stage 3, by `max(abs(dx),abs(dy))`, provided this is **<30**. This is a 59×59 square with offsets -29..29, not a circular or path-length radius. Equal distances favour the lower vegetation ID. No player/farm filter, tree reservation or nearest-reachable scoring is present.

Initial search is around the worker's current tile. If none is found and Manhattan distance from the workplace beginning point is at least six, it also searches from that workplace point. The first approach and its opposite-side alternative are attempted in order. An unreachable nearest tree does not trigger a general next-nearest-tree search in this selector.

| Pick counter | Preferred approach relative to tree | Alternative |
| ---: | --- | --- |
| 0 | (-2,0) | (+2,0) |
| 1 | (0,+2) | (0,-2) |
| 2 | (+2,0) | (-2,0) |
| 3 | (0,-2) | (0,+2) |

After each completed pick below four, search runs again from the worker's **new position**. The worker can switch trees; do not force a circuit around one tree.

### Worker states and timings

| State | Work and next step | Timing |
| ---: | --- | --- |
| 0 | Reset pick count, search, save vegetation target and issue approach order | Search occurs in the callback; transition does not execute every following state immediately |
| 1 | Idle animation, rest gate, then retry state 0 | Sequence 132 positions, interval 2: **396 normalized ticks**, subject to gates/phase |
| 2 | Walk to first approach, check tree still ripe | Travel from actual current position; B=1, U retained |
| 3 | Picking animation; increment count; re-search if <4; otherwise choose granary | 24 positions × 3: **72 normalized ticks/pick** |
| 8 | Walk to a later selected approach; recheck ripeness | Travel; possibly another tree |
| 4 | Return to workplace after partial-pick/storage failure branch | Travel and conditional return setup |
| 6 | Prepare granary delivery on fallback branch | **11 calls** from timer zero before selection; retry branches add time |
| 7 | Walk to own granary, validate and deposit apples, then return/reassign/rest | Timer gate **5 per apple**; timer runs during travel |
| 5 | Return without a pending harvest load / reassignment | Conditional return gate and travel |

Four uninterrupted picking animations contribute **288 normalized animation ticks**, but the complete load also has searches, changing approach paths, output travel, deposits and return. State 3 can send the fourth pick **directly** to state 7; do not add the state-6 11-call wait to every successful load.

Expiry branches matter: an unripe tree on the **first arrival** sends the farmer back without a load. If fruit expires during an already-running pick or before a later approach, previous picking can take the partial path through states 4/6. That path still calls the **full base-3 yield**, not one quarter per completed pick.

At P=100/M!=0, completed loads normally alternate **4 and 5 apples** through worker fractional carry. Four apples deposit at calls 5,10,15,20 when already at the valid granary with timer zero; five need 25 calls. Walking can absorb the first timer gate. The worker delivers to its own granary even if the trees belong to an enemy.

Nominal repeat structure: `home/search -> walk -> pick/reselect up to four times -> granary -> deposit -> home/rest/search`. Do not sum 288 plus tree growth time for each load: tree growth runs independently while workers travel/work.

Staggering orchard construction by about 1,000 ticks lets farmers share the two alternating ripe cohorts. Two ideal 1,000-tick windows in a 2,080-tick period leave 80 total unavailable ticks if they do not overlap; walking/retry state and initial progress make actual results less regular.

## Wheat farm: full working cycle

**2026-10-20 calculation:** [Wheat cycle comparison](wheat_cycle_comparison.md) adds original-instruction worker/clock replays, the 144/52-position idle sequences, a full state-by-state elapsed cycle and numerical comparisons with the empirical 6,950+336d formula. Walking remains an explicitly synthetic integration assumption.

Building type 30; worker type 11. Worker `0x133DD0`, building `0xA36D0`, crop pulse/priority `0xC8840`, field target selector `0xBBE00`.

### Field representation and search

One 3×3 cabin sits in a 9×9 marked area. There are **36 productive crop cells**, stored as tile IDs at building +0x1C8. Two fixed layouts alternate per player via player +0x45C, current Dashboard name `N00004615`. The field is not 81 productive cells and is not 36 vegetation objects.

Unlike apple selection, wheat selection scans **its own farm's stored target list**, choosing the first cell matching the current task. It is not a nearest-cell radius search. Layout ordering therefore changes the path taken within the field. The two layouts, cabin anchor and target ordering are in [farm construction](../../shc-dashboard/SHCDE_FARM_CONSTRUCTION.md).

### Crop growth and task priority

Each building update increments a counter. When it becomes **>150**, it resets and performs the crop update: a **151-tick pulse** for that farm. Crop states live in the byte grid at RVA 0x5001730.

| State | Meaning used by the worker |
| ---: | --- |
| 0 | Unprepared |
| 1 | Prepared, available for sowing |
| 2–10 | Growing |
| 11–100 | Harvestable |
| 101–120 | Accepted by the preparation selector, with aging/clearing distinctions |
| 120 | Harvested/cleared cell |
| 121 | Bundle awaiting collection |

The pulse increments states **2 through 102**. From newly sown state 2, nine pulses reach state 11: **1,209–1,359 ticks**, depending on pulse phase. Cells are sown at different times, so their ripeness is staggered.

After updating/counting cells, the pulse chooses a cached farm task. The verified priority is:

```text
4 or more bundles                         -> collect bundles (task 8)
otherwise >11 ripe cells                  -> harvest (task 6)
otherwise ripe>0 AND cleared-state120>0    -> harvest (task 6)
otherwise at least one bundle             -> collect bundles (task 8)
otherwise prep/cleared/aged work remains   -> prepare (task 3)
otherwise prepared-state1 cells remain    -> sow (task 5)
otherwise                                -> wait/home (task 2)
```

The exact aging counters preserve additional distinctions in the native code. The saved tests cover empty, prepared, growing, aged, ripe/cleared and bundle combinations. In particular, **11 ripe plus growing cells can select waiting; 12 ripe selects harvesting**. The task is not freshly recomputed from scratch by every worker action; the field selector reads actual cells under the cached task.

### Worker states and timings

| State | Work | Timing/boundary |
| ---: | --- | --- |
| 0 | Initial return/setup | Conditional worker setup, not a whole field cycle |
| 1 | Idle animation/rest and next task search | Sequence/phase-dependent; not a retry every tick |
| 2 | Return/wait at workplace and task handling | Conditional 20-call return gate, timer state matters |
| 3 | Walk to preparation target | Uses actual field target; path can be trimmed at the target |
| 4 | Prepare the cell | 16 positions × 3: **48 normalized ticks**; writes state 1 at animation index 8 |
| 5 | Walk to prepared cell and sow | On arrival writes state 2; **no separate 48-tick sowing animation** in this branch |
| 6 | Walk to harvest target and set facing | Actual current position -> selected crop |
| 7 | Harvest animation and choose next harvest/bundle task | 16 positions × 3: **48 normalized ticks** |
| 8 | Walk to bundle; collect on arrival | Writes its cell from 121 to 120; goes to 9 |
| 9 | Find wheat stockpile and prepare yield | **11 calls** from zero timer on immediate success; path failure can set timer -50 |
| 10 | Deliver carried wheat | **5-call gate per unit**, timer can advance during walking |

At harvest animation index 1, if the crop has not already been marked harvested, the counter increments. The first two crops become 120; every third becomes 121 and the three-crop counter resets. This write happens early in the animation, not only at its end.

A collected bundle calls yield with **base 2, bonus flag 0**. At P=100 that is two wheat. A clean 36-cell harvest can therefore create **12 bundles / 24 wheat**; at P=150 it yields 36 wheat. This describes a complete clean field's output, not a promise that all deliveries fall into one fixed interval. Old cells, stock limits, interruptions and rest affect the trace.

For all 36 cells, preparation and harvesting animations alone contribute `36 * (48 + 48) = 3,456` normalized ticks. Internal walking, sowing arrivals, target transitions, pulse phase, waiting, bundle collection, output searches and deposits add time. Growth overlaps other work. **Neither 3,456 nor 3,456 + 1,359 is a complete farm-cycle constant.**

After unloading, the worker can select active harvest/bundle work and route **directly from the stockpile to that crop target**. Otherwise it returns to the workplace/rest path. Consequently, twelve identical “cabin -> stockpile -> cabin” round trips are not the exact native delivery geometry.

Speed: B=1, retained U, so 16 ticks/tile at U=0. The checked callback does not introduce a generic loaded/empty speed switch. It is especially important to validate U provenance against the user's measured 12-tick empty return before choosing a default profile.

## Mill: three carriers sharing one processor

Building type 19; worker type 15. Worker `0x1377C0`, building `0xA43C0`.

The mill has **three millers** and one active processing-worker slot (building +0x1C0). Transport can overlap, but only the worker holding that slot processes. A waiting miller also checks processing phase zero before acquiring it. Do not assume a separate machine per worker or impose an invented FIFO queue: arrival timing and native unit iteration order determine who claims the available slot.

| State | Behavior | Timing/speed |
| ---: | --- | --- |
| 0/1 | Setup/idle animation/rest, then search wheat input | Idle sequence/context-dependent |
| 2 | Empty return to workplace | **U=1, 12 ticks/tile**; conditional return setup |
| 3 | Empty walk to selected wheat store; validate; remove one wheat | **U=1, 12** until pickup; sets U=0 at pickup |
| 4 | Carry wheat to mill; claim processor or enter wait state | **U=0, 16**; conditional return timer |
| 6 | Wait for active slot=0 AND processing phase=0 | Variable; other workers can still transport |
| 5 | Own shared processing slot; wait for phase 3 | **272 normalized building ticks** from fresh aligned phase-0 body setup |
| 7 | Post-processing worker index gate, select flour stockpile | Interval 0 and animation-index gate **20**; preserve incoming animation pulse/index phase |
| 8 | Carry one flour to stockpile, validate/deposit, then seek wheat or return home | **U=0, 16**; ordinary successful flour transfer can happen immediately after arrival/zero retry timer |

Processing runs on **every second building update**:

```text
phase 0 initialization:           1 body call
phase 1 sequence + terminator:  105 body calls (104 positive positions)
phase 2 sequence + terminator:   30 body calls (29 positive positions)
total:                         136 body calls = 272 simulation updates
```

Building and worker handoff phase can shift observed start/finish boundaries; initial half-rate residue and queue occupancy matter. The 300 counter in the mill is a wheel/coasting parameter, not a 300-tick grinding timer.

New original-instruction fixtures verified phase-0 processing to the worker's state-7 transition in **272 updates**, with the chosen synthetic half-rate clock alignment. They also checked productivity 100, 150 and 200: wheat pickup removes **one actual stock wheat**; at P=200 the yield helper temporarily sets carried wheat to two, but state 7 explicitly sets departure carry to **one flour**, and the ordinary delivery deposits one flour. Do not multiply flour output by the temporary wheat-carry bonus.

The repeated route can be `flour stockpile -> wheat stockpile -> mill/queue/process -> flour stockpile`. Empty legs have the explicit 12-tick coefficient; loaded wheat/flour legs have 16. Add the actual state gates, shared processor handoff and storage retries. A throughput bound such as `min(3 / workerCycle, 1 / processorSpan)` is useful for checking an implementation, but cannot reproduce queue phases or startup by itself.

## Bakery: flour input to bread delivery

Building type 17; worker type 16. Worker `0x138850`, building `0xB1820`.

| State | Behavior | Timing/boundary |
| ---: | --- | --- |
| 0/1/2 | Setup, home/idle/rest, select flour input | Conditional return gate; idle sequence/context matters |
| 7 | Walk to flour stockpile; validate; remove one flour and return | Native input ID order, not minimum complete loop |
| 8 | Walk to bakery; on arrival enter state 109, resume state 4 | B=2 / retained U; **24 ticks/tile if U=0** |
| 109, entering | Fade/transfer into building | **17 worker calls** from zero timer |
| 4 | Start building action/sequence | A separate worker dispatch |
| 3 | Poll completion | **1,632 normalized building animation ticks**, plus handoff phase effects |
| 109, leaving | Prepare output, leave building; resume 5 | **17 worker calls** |
| 5 | Select own granary and issue route | **11 calls** from zero; initializes output timer to **-10** |
| 6 | Deliver bread one unit at a time; choose flour again or return | First deposit at **15 calls** from timer -10 if already arrived; subsequent units every 5 |

The bakery uses one flour input per ordinary pickup and calls output yield with **base 8, bonus flag 0**. At P=100: eight bread; P=150: twelve bread. Fractional carry is shared worker state and can also be touched by input yield calls; model the calls, not a floating-point batch average if exact finite-run output is required.

For eight bread, a valid zero-walk state-6 fixture deposited at calls **15,20,25,30,35,40,45,50**. For N output bread under those starting conditions: `15 + 5*(N-1) = 5N + 10` worker calls. This is not always added on top of travel: the timer increments while the baker is walking. Failed retargeting can initialize other negative timers.

After unloading, native input selection can send the baker directly to flour storage. If that fails, it can return/reassign/rest at its workplace. The current app's route order resembles the successful native loop, but shortest-granary optimization, a universal 1,700 work ticks and omitted inventory/handling are approximations.

## All crafting workshops

The ordinary supported crafting industries are fletcher, poleturner, blacksmith, armourer, tanner and brewer. Bakery is covered separately above. Every workshop has an input acquisition path, a worker/building handshake and output delivery. Existing simulator aggregate work times must be retained as calibration data rather than renamed to raw animation durations.

### Normalized building animation table

| Product | Input per normal processing operation | M != 0 | M = 0 | Current app's empirical work ticks |
| --- | --- | ---: | ---: | ---: |
| Bow | 2 wood, fetched individually | **581** | 1,324 | 625 |
| Crossbow | 3 wood, fetched individually | **509** | 1,664 | 555 |
| Spear | 1 wood | **246** | 510 | 290 |
| Pike | 2 wood, fetched individually | **543** | 1,533 | 872 |
| Sword | 1 iron | **1,044** | 1,732 | 1,090 |
| Mace | 1 iron | **856** | 1,540 | 910 |
| Metal armour | 1 iron | **512** | 1,852 | 585 |
| Leather armour | 1 local hide | **1,341** | 2,320 | Not implemented |
| Ale, full ordinary phase 0–7 pass | 1 hops for the normal single-input pass | **2,672** | 2,672 | Not implemented |

These are index/phase durations, not complete worker cycles. Bow is `145*4+1`, crossbow `127*4+1`; spear has 104+19 indices at interval 2, pike 181 at interval 3, sword 261 at interval 4, mace 214 at interval 4, metal armour 128 at interval 4. Leather uses 335 positions at interval 4 plus its immediate final phase. Brewer sets interval 0, effectively one eligible step/update, while some phases hold table entries via index division by four.

### Handshake and worker gates

Building animation advances on **>= interval**; worker animation advances on **> interval**, giving D+1. Building updates precede worker updates in the traced active simulation path. A completion latch and actions 0/1/2/3 coordinate stop/poll/restart/freeze. Starting a new sequence does not reset every animation clock field. Readiness and handling can therefore change normalized boundaries by a few ticks.

New native state-boundary fixtures established:

| Worker | Preparation state | Calls from timer 0 | Entry/exit transfer on normal processing route | Output-destination state / gate |
| --- | ---: | ---: | --- | --- |
| Fletcher | 5 | **30** | No generic 17-call fade pair in the traced ordinary route | 6 / **11** |
| Poleturner | 6 | **30** | State 109, **17 entering + 17 leaving** | 7 / **11** |
| Blacksmith | 6 | **30** | Direct hide/show calls in this route; do not add the other workers' fade pair | 7 / **11** |
| Armourer | 6 | **30** | State 109, **17 + 17** | 7 / **11** |
| Tanner | 6 | **30** | State 109, **17 + 17** around the processing path | 7 / **11** |
| Brewer | 6 | **30** | State 109, **17 + 17** around the completed output path | 7 / **11** |

Output deposits generally use a 5-call short-timer gate per item, with arrival/capacity validation and retargeting branches. Preserve the timer **assigned by that specific caller**. Brewer, like baker, assigns -10 on its output departure; it must not inherit fletcher's zero-start deposit timing. Several local-buffer workers can continue another input/processing operation before taking finished local output; do not force a delivery after every animation terminator.

### Fletcher

Sequence: find wood -> pick up one -> return/deposit -> repeat until 2/3 local wood -> 30-call preparation -> start building craft -> poll completion -> yield -> 11-call armoury selection -> walk/deposit -> return to workshop or take the advanced direct-input shortcut.

The crossbow zero-distance native replay gives actual crafting-state occupancy **507–510** ticks depending on clock residue, around the normalized 509. With ordinary return, N output items, no rest/failures and co-located endpoints, repeated-cycle elapsed ticks are:

```text
A = actual crafting-state calls
elapsed       = A + 51 + 5*N
native ring   = A + 38 + 5*N
```

First batch omits one preceding return call. The advanced shortcut changes the repeated successful formulas to elapsed `A+49+5N` and ring `A+37+5N`. One tested first batch took **563 elapsed ticks but recorded 550**. This is why neither 509 nor the game's worker average can directly replace the current calibrated 555. [Full replay explanation](../../shc-dashboard/SHCDE_FLETCHER_CYCLE_TIMING.md).

### Poleturner

Select wood -> collect one -> deposit at workplace. Spear needs one local wood; pike needs two and repeats the acquisition trip. Once enough local input exists, enter state 109/resume 6, perform the 30-call gate, consume local input/start craft, poll state 5, then leave through state 109/resume 7, select armoury and deliver.

The building overwrites its animation interval to **2 for spear / 3 for pike**, so modifying only a default interval table is insufficient. Use the selected versus currently working product fields and native phase sequence. The empirical pike 872 versus normalized 543 is a particularly important manual validation case; do not discard the old value or assume the difference is a known fixed overhead.

### Blacksmith

Select iron -> validate/pick up one -> return/deposit one local iron and hide -> 30-call preparation -> consume/start selected sword/mace -> poll completion -> stop/show and yield -> 11-call armoury selection -> travel/deposit -> next input or return/rest. The ordinary hide/show branch does not use the 17+17 state-109 pair seen in poleturner/armourer. Both processing and input/output timing are product/state dependent despite the same B=2 walking coefficient.

### Armourer

Select iron -> pick up/return/deposit -> 17-call entry -> 30-call preparation -> consume iron/start -> poll the 512-normalized-tick animation. Its local finished-armour buffer and remaining local input decide whether another processing operation starts or finished output is collected. The output route uses 17-call exit, 11-call armoury selection and per-item deposits. With the usual one-input path this produces the familiar one processing operation between input and output, but the native local buffers should still be represented.

### Tanner and cow supply

Tanner uses **local hides**, not a standard input stockpile recipe. If hides exist it can enter the work path. Otherwise `0x18B470` scans unit slots for a living **own cow (chimp 51)** satisfying its availability checks, including AI state zero and an additional zero-valued context field. It chooses nearest Chebyshev distance with a strict best comparison, hence lower ID on a tie. The examined helper has no orchard-style 30-tile cutoff; its initial best is 10,000.

The worker validates the cow's slot/global ID, approaches and coordinates the cow's return to the workplace, then runs the slaughter action. Sequence RVA **0x332B88** has **30 positive positions**, with unit interval 3: **120 normalized animation ticks**. During this sequence it changes the cow state; on completion it adds **three local hides**, then enters state 109/resume 6. A new original-instruction boundary fixture confirmed that three-hide addition and next-state pair.

For each hide: 30-call gate -> consume one hide/start -> 1,341 normalized animation ticks -> collect leather output -> 17-call exit -> 11-call armoury selection -> deposits/return. The stock/growth/location/availability of cows adds variable time before those three processing opportunities. A dairy/tanner simulator must share actual cow entities, rather than giving both industries an unlimited independent cow supply.

### Brewer

Select hops -> validate/pick up one -> return/deposit local hops -> 17-call entry -> 30-call preparation -> consume input/start -> run building phases -> add local ale -> return through the preparation/local-buffer decision. If another local input is present it can process again; otherwise take finished local ale, apply yield, exit through the 17-call transfer, select stockpile and deliver.

The full ordinary reset-to-zero processing pass is **2,672 normalized ticks**. A building completion branch can set phase 4 in M!=0, but the worker's action-2 restart resets phase to zero: do not infer that phases 4–7 alone describe every normal batch. Output selection uses stockpile rules and an ale cap; output timer starts at -10. Speed is B=1/retained U, typically 16 at U=0.

## Yield, productivity and rest

Shared yield helper `0x18D940` reads player `r_ProductivityPercentage` (+0x211C). For ordinary positive values without integer overflow:

```cpp
p = max(player.productivity, 100);
if (M != 0 && eligibleModeBonus) p += 50;
accumulator = (p - 100) * baseAmount + worker.remainder;
extra = accumulator / 100;
worker.remainder = accumulator - extra * 100;
return baseAmount + extra;
```

The extra 50 is **percentage points before quantization**, not multiplying a fear-adjusted result by 1.5. Keep the remainder per worker, including all its input/output yield calls.

| Output call | Base / mode-bonus flag | P=100, M!=0 | P=150, M!=0 |
| --- | --- | --- | --- |
| Wheat bundle | 2 / 0 | 2 | 3 |
| Bread | 8 / 0 | 8 | 12 |
| Weapon/leather/ale | 1 / 1 | 1,2 alternating from zero carry | 2 |
| Apples/cheese | 3 / 1 | 4,5 alternating | 6 |
| Hops in M!=0 | 2 / 1 | 3 | 4 |

The current ten-cycle fear ring can match an average while distributing outputs among cycles differently from native fractional carry. Delivery time depends on the actual integer output, so that difference matters to finite runs.

Below 100 productivity, the yield helper clamps its input. Rest implements the loss instead. Rest setup uses the worker's recorded cycle average and context gates, approximately `floor((100-P)*recordedAverage/100)` in the ordinary range, adjusted by rest credit. AI state 121 and path/arrival gates govern actual resting. Do not scale every animation duration by `100/P`.

The production ring is four helper-visit counters. Some states omit recording and some completion calls add a second count. Keep **elapsed ticks**, **delivered output**, **native counted average** and **rest time** as distinct metrics.

## Other industries and scope of “all workshops”

The crafting workshops above are covered individually. These related production buildings can reuse the movement/storage/yield infrastructure, but some have additional integration work before claiming exact whole-industry throughput:

| Industry | Established timing/behavior | Integration still needed |
| --- | --- | --- |
| Woodcutter | B=2/retained U; 384-tick normalized sawing; separate 48-call between-log wait; base-12 eligible output gives 18 at P=100/M!=0 | Tree choice/felling/log extraction and complete routes |
| Hunter | 896-tick normalized processing; base-6 eligible meat gives 9; chase changes B with distance; returning animal B=2/U=0, meat delivery B=1/U=0 | Moving animal target, chase/shoot hit timing |
| Pitch digger | 1,088-tick normalized processing; B=1/U retained; base-1 eligible output | Full terrain-target/return traces |
| Hops farm | 24 crop cells; 401-tick pulses; state 2 -> ripe 14 needs 12 pulses, 4,412–4,812 ticks; 96 normalized ticks planting/picking; six picks/load | Full field task/route migration, including mode base change |
| Dairy | Separate cows; 401-update approach gate plus half-rate milk phases; base-3 eligible cheese output | Shared cow lifecycle and complete timing with tanner interference |
| Iron mine / quarry / ox | Multiple worker roles, local buffers and transport; ox U=-1 pauses are explicit | Existing 2.63 iron/min / 18.6 stone/min rates remain empirical; not certified exact whole cycles |

## Direct comparison with the current Rust simulator

The source was inspected in its existing modified working tree; no Rust or GUI behavior was changed. The full current calibration snapshot is [preserved here](backups/empirical_model_before_native_upgrade_2026-10-19.json), including source hashes. The older 2026-10-14 weapon backup remains intact.

| Current code | Current behavior | Native-model requirement |
| --- | --- | --- |
| `Simulator::tick`, simulator.rs | Advances `GameTime` only | Advance worker/building/resource state in deterministic order |
| production_cycle.rs | Empirical make ticks plus path-length × fixed speed | Explicit acquisition, preparation, animation clocks, output/retry/return states |
| production_cycle.rs fear ring | Average output from a ten-cycle array | Per-worker integer remainder and actual deposit count |
| bread.rs farm | `6950 + 12 * distance * (16+12)` | 36 cells, pulse/task state, internal routes, bundles and state-specific delivery origins |
| bread.rs mill | 312 processing ticks, min of three-worker and processor rates, all travel at 16 | Shared slot/phase, 272-normalized processing plus worker gates, empty 12 / loaded 16 |
| bread.rs bakery | 1700 work ticks; choose shortest granary loop | 1632 animation, separate 17/17/11/negative-timer transfers, native storage IDs |
| entry_logic.rs | Recalculate square entry; radius-6 escape check; simplified one-wall priority | Cached access retention, keep components/connection route, exact two-wall priority, 3×3 farm cabin anchor |
| footprint.rs | 9×9 wheat area with a corner cabin | Good separation already exists; add real crop targets and align Y-up UI origin to native Y-down beginning coordinates |
| pathfinding.rs | Uniform eight-neighbour BFS, NW-first loop; stores length only | Native masks/profiles and search order, +1/+2 labels, routes/substeps, terrain/crowd when enabled |
| input stock helper | First matching designated stockpile | Goods/capacity/owner/slot-order selection; existing games can have several eligible stores |
| BuildingType | Wheat/bread and seven weapon products, no orchard/tanner/brewer | Add missing types as explicit phases, not fixed income numbers |
| project.rs v2 | Geometry/settings; no scheduled placement/worker inventory state | Explicit model version, seed/initial conditions and optional runtime snapshot in a future schema |

The current model is valuable calibration evidence. A native model should expose the differences and be checked against it and game delivery traces. Substituting 509 for 555, 272 for 312 and 1632 for 1700 while retaining the same aggregate formulas would mix incompatible definitions.

## Remaining validation items

1. Complete initial/reassignment U provenance for ordinary farmers and retained-speed workers; measure the user's empty-return case against actual B/SB/U and travel endpoints.
2. Identify native M and related flags in each intended game mode without guessing menu names.
3. Full startup/queue phase traces with three millers, storage shortages/capacity changes, and loaded worker reassignment.
4. Live per-stage timing for the unusually different pike calibration and all products after explicit handling states are represented.
5. Complete vegetation/building/unit update order, skip gates and initial phase when reproducing first-tick boundary behavior.
6. Exact map coordinate import, special terrain/path profiles, all retry/failure states and crowd effects before advertising whole-game fidelity.

The verified rules are sufficient to implement and independently test a much richer simulator. These remaining details should have visible assumptions and comparison fixtures, rather than being hidden in a newly fitted constant.

## Source handoff

Primary related reports: [production](../../shc-dashboard/SHCDE_PRODUCTION_TIMING.md), [worker movement/searches](../../shc-dashboard/SHCDE_WORKER_MOVEMENT_AND_TARGETS.md), [farm construction](../../shc-dashboard/SHCDE_FARM_CONSTRUCTION.md), [fletcher timing](../../shc-dashboard/SHCDE_FLETCHER_CYCLE_TIMING.md), [entrances](../../shc-dashboard/SHCDE_BUILDING_ACCESS_POINTS.md).

New original-instruction probe: `C:\repositories\shc-dashboard\artifacts\production-static\verify_production_boundaries.py`. It verifies 22 worker gates, three deposit sequences, three mill/productivity cases, ten wheat priority cases and the tanner slaughter boundary. The durable [evidence JSON](research/native_production_evidence_2026-10-19.json) includes those results and probe hashes. Native decompilation/assembly remain in that directory's `decompiled` folder; original instructions are authoritative over provisional Ghidra names/types.
