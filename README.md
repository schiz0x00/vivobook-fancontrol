# vivobook-fancontrol

Custom fan control for the ASUS Vivobook Pro 15 (N6506MU) on Linux, with
Secure Boot left on.

The firmware only exposes three thermal profiles. Under the hood, each
profile switch makes ACPI write fan tables into EC RAM; this project
reverse-engineered those tables (see `NOTES.md`) and writes its own through
the firmware's own EC mailbox methods (`RRAM`/`WRAM` via `acpi_call`).

## What you get

- **vfcd** — root daemon. Owns all EC writes, keeps your fan settings in
  place (the firmware rewrites the tables on every profile switch; vfcd
  re-applies within ~3 s), applies a temperature guard (custom fans go to
  full speed at 95 °C until < 80 °C, configurable), restores the stock tables
  when stopped, and serves a socket (`/run/vfcd.sock`, group `sudo`).
- **vfc-gui** — desktop app:
  - Dashboard: live RPM / target / level / duty per fan, CPU + EC temps,
    10-minute graphs.
  - Fan control: per fan *Stock*, *Fixed speed* (incl. off) or *Curve*
    (drag points on the graph, up to 13), hysteresis, presets, "use at boot".
  - EC tables: decoded per-level editor + raw hex editor for 0x0A00–0x0AFF.
  - EC explorer: read any EC RAM range, live refresh, change highlighting.
  - System: platform profile, temperature guard, battery charge limit,
    dGPU power (via `dgpu-power`).
  - Log: daemon events (firmware resets, guard trips, errors).
  - `vfc-gui control|tables|explorer|system|log` opens on that tab.
- **vfc** — CLI: `vfc status`, `vfc set 0|1|all <rpm|off|stock>`, plus the
  research commands (`read`, `tables`, `scan`, `diff`, `extract`, ...).

## Install

```sh
cargo build --release
pkexec "$PWD/install.sh"   # binaries -> /usr/local/bin, enables vfcd.service
```

Needs the `acpi_call` DKMS module signed with an enrolled MOK (Secure Boot /
lockdown). The installer adds it to `/etc/modules-load.d`.

## Safety

- Writes are limited to the fan table block the firmware itself writes
  (0x0A00–0x0AFF) plus the profile mode code; every write is read back.
- The guard overrides custom modes at high temperature; stock fans are never
  touched. CPU thermal throttling and the EC's own shutdown stay in place.
- "⟲ Stock" (GUI), `vfc set all stock`, or stopping vfcd brings back the
  firmware tables (via a platform_profile switch).

## Layout

- `src/tables.rs` — table layout + encoding of fixed speeds and curves
- `src/bin/vfcd.rs` — daemon; `src/proto.rs` — socket protocol
- `src/bin/vfc-gui/` — egui app; `src/main.rs` — CLI
- `NOTES.md` — findings; `acpi/tables/` — ACPI dumps; `dumps/` — EC dumps
