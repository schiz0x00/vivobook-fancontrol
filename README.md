# vivobook-fancontrol

Custom fan curves for the ASUS Vivobook Pro 15 (N6506MU) on Linux.

The firmware only exposes three thermal profiles (quiet / balanced /
performance). Under the hood, each profile switch makes ACPI write a set of
fan tables into EC RAM. This project reverse-engineers those tables and writes
its own, through the firmware's own EC mailbox methods, with Secure Boot left on.

Status: **research** — both fans controllable live by writing EC table entries (tach target = setpoint); curve semantics mostly mapped, no daemon yet.

## Layout

- `NOTES.md` — findings: EC mailbox protocol, fan readout, profile switch, table layout
- `acpi/tables/` — raw ACPI tables from `/sys/firmware/acpi/tables` + `iasl -d` output
- `src/` — `vfc`, a Rust CLI: `extract` (DSDT tables), `read`/`fans`/`tables`/`snapshot`/`scan`/`diff` (EC reads), `write`/`apply`/`restore` (table writes, guarded) via acpi_call
- `dumps/` — live EC RAM dumps (per profile / load)

## Approach

1. `acpi_call` (DKMS, signed with a local MOK) to call `\_SB.PC00.LPCB.EC0.RRAM`,
   `WRAM` and `FS_C` from userspace. Lockdown allows signed modules.
2. Read-only: dump EC RAM 0x0A00–0x0AFF and live fan state while switching
   profiles and loading the CPU, to confirm what each table row means.
3. Write: tweak one entry, observe, then build a proper fan-curve tool
   (ideally a small hwmon driver exposing `fan*_input` / `pwm*_auto_point*`).

## Safety

- Never write EC RAM outside the ranges the firmware itself writes.
- Restore a profile via `platform_profile` to make the firmware rewrite all
  tables from scratch if anything looks off.
- CPU thermal throttling and the EC's own shutdown stay in place regardless.
