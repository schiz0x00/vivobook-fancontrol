# ASUS Vivobook Pro 15 N6506MU — EC fan control RE notes

BIOS: N6506MU.313 (2025-10-13). Tables in `acpi/tables/`, decompiled with `iasl -d DSDT`
(combined `-e SSDT*` fails on a duplicate `_UPC` object).

## EC access

`EC0` (PNP0C09) is a stub — no standard EC region. ASUS uses memory-mapped
windows instead:

| Region | Address        | Purpose |
|--------|----------------|---------|
| ECW1   | 0xFE0D0380 +0x80 | status: battery, thermal limits, fan tach (F0TS/F1TS @ +0x6A/+0x6C?), F0CS/F1CS |
| ECW2   | 0xFE0D0700 +0x80 | command mailbox: ESEM, CMD1, CDT1.., EDA1.. |
| ECW3   | 0xFE0D0CF0 +0x80 | (not yet looked at) |

Mailbox protocol (`\_SB.PC00.LPCB.EC0.RRAM` / `WRAM`, serialized on mutex `MUEC`):
- read:  CDT1=addr>>8, CDT2=addr&0xFF, CMD1=0x80, ESEM=0x0F, poll CMD1==0, result EDA1
- write: same + CDT3=value, CMD1=0x81

Prefer calling the ACPI methods (RRAM/WRAM) over poking the window directly, so
the firmware's MUEC mutex is respected.

## Fan readout

- `FS_C(n)`: RPM = 0x1C8402 / (2 * (~FnTS & 0xFFFF)); 0xFFFF -> 0 RPM
- `M633`: fan0 -> FANS=RRAM(0x03EC), tach F0TS, TRGG=RRAM(0x0576)
          fan1 -> FANS=RRAM(0x03ED), tach F1TS, TRGG=RRAM(0x0577)

**ECW1 is a direct view of EC RAM**: 0xFE0D0380 + off == EC RAM 0x0380 + off.
So F0TS = EC RAM 0x03E8 (u16 LE), F1TS = 0x03EA, F0CS = 0x03EC, F1CS = 0x03ED.

**FS_C is buggy on this EC** (confirmed live 2026-10-26): the tach word is 0
when stopped and ~600 at level 7, *not* inverted. FS_C's `~` turns 611 into
64924 -> "14 RPM". Correct: RPM = 0x1C8402 / (2 * tach) = 934401 / tach
(~1530 RPM at level 7). `vfc` reads the words directly.

## Live behaviour (acpi_call, 2026-10-26, balanced, all-core load)

- 0x0576/0x0577 ("TRGG") = per-fan control temperature, tracks CPU pkg temp
- 0x03EC/0x03ED (F0CS/F1CS) = current curve level 0..12
- Level steps by **one level per ~3 s** towards the highest level whose
  up-threshold (0x0A00 row) <= temp; balanced capped at level 8 (90 °C)
- Going down it holds: level 8 stayed at 63 °C; down-threshold row 0x0A20
  (level 8 -> 61 °C) is the hysteresis
- Tach at level 7: fan0 611..627 vs table 0x0AC0 level-7 target 599 ->
  EC closes the loop on tach targets per level
- Live tables 0x0A00-0x0A0C etc. match the DSDT exactly for all profiles;
  bytes 0x0A0D-0x0A0F are never written (leftovers)
- Other movers seen in full-RAM scan (dumps/off-*.bin, on-*.bin):
  0x0568/0x056C (0 off, 0x7c/0x76 on -> current duty?), 0x0560-0x0565,
  0x0574-0x0587 (temperature history ring), 0x0550-0x055B (temps), 0x0907-0x090F

## Profile switch: WMI DEVID 0x00110019 (THROTTLE_THERMAL_POLICY_VIVO)

`WMNB` DEVS -> `FANL(mode)`; DSTS -> `GQFS()`. Linux asus-wmi drives this via
platform_profile (quiet/balanced/performance).

`FANL`:
- WRAM(0x08F4, code)  code: 1=standard, 2=whisper, 3=performance, 4=full
- calls FSMD/FWMD/FPMD/FFMD (writes fan tables, below)
- WRAM 0x05C2/0x05C3/0x05C5: whisper 0x3C/0x37/0x03, others 0x62/0x5A/0x05 (limits? TBD)
- WRAM(0x0BEA, 1); 0x0BE7 |= 1   (apply/commit flags?)
- Notify IETM 0x88, NPCF 0xC0

## Fan tables in EC RAM (rewritten on every profile change)

Two identical blocks, likely fan0 (0x0A00..) and fan1 (0x0A40..), 13 levels each.
Whisper caps everything from level 6 up; performance/full raise the top levels.

| Block  | Contents (standard)            | Hypothesis |
|--------|--------------------------------|------------|
| 0x0A00 | 70 73 75 80 82 84 86 88 90 90.. | per-level threshold (°C?) fan0 |
| 0x0A10 | 10 10 10 14 15 16 17 18 19 19.. | per-level ? fan0 |
| 0x0A20 | 53 54 55 56 57 58 59 60 61 61.. | per-level threshold (°C?, 2nd sensor or hysteresis) fan0 |
| 0x0A30 | 9 2 2 2 ...                     | ? |
| 0x0A40..0x0A70 | same as 0x0A00..0x0A30   | fan1 |
| 0x0A80 | 0 0 0 0 93 110 124 136 152 152.. | **duty 0-255 per level, fan0** (full: top=255, perf 200, whisper 124) |
| 0x0A90 | 1 1 2 3 x4                      | ? (sensor map?) |
| 0x0AA0 | 79 84 94 100 100 100 102 121 124.. | **duty per level, fan1** |
| 0x0AB0 | 1 1 2 3 x4                      | ? |
| 0x0AC0 | 13 x u16 LE: 0 0 0 0 829 719 653 599 553.. | **tach targets per level, fan0** (same unit as 0x03E8; matched live) |
| 0x0AE0 | 13 x u16 LE: 770 719 653 616 616 616 599 513.. | **tach targets per level, fan1** |

Confirmed live: 0x0A00 = up thresholds (°C), 0x0A20 = down thresholds (°C),
0x0A80/0x0AA0 = duty per level, 0x0AC0/0x0AE0 = tach targets per level.
Still unknown: 0x0A10 (10..19), 0x0A30 (9,2,2..), 0x0A90/0x0AB0 (1 1 2 3 x4).

All hypotheses — to be confirmed by live reads (RRAM) while switching profiles
and loading the CPU.

## Write experiments (2026-10-26) — EC does NOT pick up table edits live

All writes via WRAM, read back OK, undone with `vfc restore` (profile toggle
makes FANL rewrite everything).

1. Idle (level 0, ~50 °C): fan0 L0/L1 duty 0->93, target 0->829. No effect.
   + commit flags (`vfc apply`): no effect.
2. Idle: fan0 L1 up-threshold 73->45 (below current temp) + the above. Level
   stayed 0. + commit flags, + rewrite mode code 0x08F4=1 (same value): no effect.
3. Load: fan0 L4-L8 duty -> 200, target -> 400 (~2300 RPM). Fan0 stayed at
   tach 0 through levels 3-5, started only at level 6 (~86 °C), ramped slowly
   500->1400 RPM by t=75s — same behaviour as stock. User confirmed by ear:
   kicked in at ~80 °C "like before, not louder". Duty bytes stayed c8 (EC
   didn't touch them), unlike the stock run where it nudged the active entry.

Other observations:
- Right after `restore`, 0x0A86 read 0x7b although FANL writes 0x7c -> the EC
  does write into this block at some point (around profile switches).
- Commit flags: 0x0BE6=0x81, 0x0BE7=0x81, 0x0BEA=0x01, 0x0BEB=0x01, persist;
  EC doesn't clear them -> `apply` (|= 1) never produced a 0->1 edge.
- Fan0 starting only at level 6 (not 4 as the 0x0A80 duty row suggests) means
  level<->table index mapping is not fully understood yet.

4. **BREAKTHROUGH — fan1 tables ARE live.** Idle, fan1 (tach 0x03EA, always
   on) at level 1, 1210 RPM. Wrote fan1 L0-L3 duty (0x0AA0..) = 200 and tach
   target (0x0AE0..) = 450. Within 3 s, *before any trigger*: tach 772 -> 324,
   **1210 -> 2883 RPM** (user heard fans go to full at low CPU load). The EC
   then walked the active level's duty down c8 -> c7 -> c5 -> c2 -> c1, i.e.
   closed-loop duty trimming towards the target. Mode code 1->3->1 afterwards
   changed nothing further. `vfc restore` brought everything back.
   Tests 2 (flag edge) and 3 (race) were stopped as moot.

So: **0x0AA0 (duty) / 0x0AE0 (tach target) drive the fan on tach 0x03EA and
are read live.** The duty row is a starting point the EC trims to hit the tach
target (that's the 7c -> 7a -> 74 drift seen in the stock scans).

5. **Fan0 is live too** (after reboot, fresh acpi_call). All-core load until
   fan0 spun up on stock tables (85 °C, level 6, 478 -> 797 RPM ramping).
   Wrote fan0 duty = 200 / target = 400 for all 13 entries. 3 s later
   **2208 RPM**, settling at ~2280 RPM (tach 408-410 vs target 400). The EC
   raised the active entry's duty c8 -> cb -> d3 -> d4 to hit the target.
   User: "way louder at 80 °C than before". Script auto-restored on exit.

**Table index = level - 1.** At level 6 the EC trimmed entry 5 (0x0A85); in
the stock scan at level 7 it trimmed entry 6 (0x0A86). Level 0 = fan0 off
(no table entry used) — which is why the idle fan0 tests (level 0) did
nothing. Stock fan0 duty entries 0-3 are 0, entry 4 is the first non-zero ->
fan0 starts at level 5 (~80-85 °C), matching what the user hears.

**Control model:** temperature (0x0576/0x0577) vs up/down thresholds picks the
level (1 step per ~3 s); level-1 indexes the duty/target rows; the EC starts
from the duty entry and trims it until the tach matches the target entry.
The **tach target is the real setpoint.**

6. **Threshold rows are live.** fan0 up row 0x0A00 -> 40..52, down row
   0x0A20 -> 35..47 (stock duty/targets).
   - Idle, temp flat at 47 °C for 30+ s: level stayed 0 for 45 s.
   - Light load, temp wobbling 45-47 °C: 9 s after the write fan0 jumped
     **level 0 -> 8 in one step** (= number of up thresholds <= 47), started
     spinning ~9 s later at **46 °C** and ramped 500 -> 1350 RPM. Fan1 stayed
     at level 0 -> 0x0A00/0x0A20 are fan0-only (fan1: 0x0A40/0x0A60).
   - Working theory: the EC re-evaluates the level on temperature *changes*;
     a flat reading doesn't trigger it.
7. 2-core load reached 92 °C in 12 s; stock fan0 then sat at 0 RPM for 12+ s
   at level 13 (max) and needed ~45 s to reach 1800 RPM. Level can jump
   several steps at once when hot (1 -> 3 -> 6 -> 13 in 9 s).

**Level semantics:** level = number of up thresholds the control temp has
passed (0-13), held until it falls below the matching down threshold. Level 13
exists (all 13 thresholds passed) and uses entry 12.

**Fan behaviour on start:** ~9 s spin-up delay after leaving the off state,
then ramps ~+100 RPM / 3 s (EC slew limit) — even towards a far target.
Rows 0x0A10 / 0x0A30 (unknown) may be timing / slew parameters.

Still open: rows 0x0A10, 0x0A30, 0x0A90/0x0AB0; what resets the tables
(profile switch for sure; resume, AC change?).

Superseded hypothesis: "EC copies tables on a trigger" — wrong for fan1.

Next tests:
- Edge on commit flags: clear 0x0BEA / 0x0BE7 bit0, then set them.
- Mode code change without FANL: edit tables, then write 0x08F4 1->3 (and
  back), see whether the EC adopts the edited block.
- Edit tables *between* FANL's writes: set platform_profile, then immediately
  overwrite (race the EC's copy).

## Side findings

- `\_SB.ATKD.AGPU` / `RGPU`: ASUS dGPU attach/eject helpers (RP12.AGON, Notify
  PXSX eject). Possibly a firmware dGPU power path not exposed by asus-wmi.

## Plan

1. acpi_call (DKMS, signed with a new MOK) -> call RRAM/WRAM/FS_C from userspace.
   Works with Secure Boot ON (signed module; lockdown only blocks unsigned code,
   debugfs writes, ACPI table overrides).
2. Read-only phase: dump 0x0A00-0x0AFF, 0x03EC/0x03ED, 0x0576/0x0577, 0x08F4,
   0x05C2-0x05C5, tach; correlate with profile + load.
3. Write phase: modify one table entry, observe; then build a small hwmon /
   fan-curve tool.

## Daemon-era findings (2026-10-26, via vfcd)

8. **Thresholds of 0 are invalid.** Fixed mode with up = down = 0: level fell
   to 0 on every temperature *decrease*, then jumped back to 13 on the next
   increase (fans coasting down in between). Up row 0 is never passed; with
   up 20/30 and 0x0A20 row = 0 the same 13 <-> 0 flapping happened. With up 30
   and 0x0A20 = 25 the level held at 13 for 60+ s while temps fell 39 -> 36,
   both fans within 1% of target. -> no 0 in up or 0x0A20 rows.
9. **Row 0x0A10 (fan1 0x0A50) is the hysteresis, 0x0A20 is not the drop
   threshold.** Curve 30/38/45 °C, 0x0A20 = up (hyst 0): level 3 held at 36 °C.
   Moving point 3 to 50 °C: dropped to level 2 at 37 °C (~30 s later) and held
   there although 37 < 38. Fits: level L drops when temp < up[L-1] - 0x0A10[L-1]
   (45-10 = 35 holds at 36; 50-10 = 40 drops; 38-10 = 28 holds). Writing the
   hysteresis into 0x0A10 (3) settled the same curve at level 2 at 36-37 °C
   within seconds, 1611-1619 rpm for a 1600 target. Drops can lag 20-30 s
   (row 0x0A30 = 9, 2, 2.. may be the delay). 0x0A20's meaning is still open
   (stock: 53..61, below up - hyst).
10. **Fan1 stops at level 0** (and at duty 0 / target 0): fixed 0 rpm coasted
    1259 -> 0 in ~25 s and stayed there.
11. Fixed 1300/1600/2000 rpm held within ~1% (fan0 1305-1312, fan1 1288-1297
    for 1300). Starting from a running fan, the EC reached a new target in
    3-6 s; the ~100 rpm/3 s ramp seen earlier was from standstill.
12. EC reads through acpi_call: ~1.2-1.3 ms per byte (256-byte table ~0.3 s).

Encoding used by vfcd (src/tables.rs): fixed = up 20 / hyst 10 / low 10 on
all levels; curve point k -> up[k] = max(temp, 20), hyst[k] = hysteresis,
low[k] = up - hyst; unused levels up/low = 255 and repeat the last speed.
