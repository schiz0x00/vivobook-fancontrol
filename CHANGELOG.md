# Changelog

## [0.1.0] - 2026-10-26

First release, for the ASUS Vivobook Pro 15 N6506MU (BIOS N6506MU.313).

### Added

- **Fan Control** (`fan-control`): the everyday app. Quiet, Normal, Cool and
  Max in one click, "My speed" with a check against the measured RPM, and
  "My curve" with validated "at X °C run Y rpm" lines. Choices persist across
  reboots.
- **vfc-gui**: advanced app with live graphs, per-fan stock / fixed / curve
  modes, a draggable curve editor, presets, decoded and raw EC table editors,
  an EC RAM explorer, platform profile, temperature guard and charge limit.
- **vfcd**: root service owning all EC writes. Re-applies settings after
  firmware table rewrites, full-speed guard at 95 °C, restores the stock
  tables on stop.
- **vfc**: `status` / `set` CLI plus the research commands used to map the EC.
- Board check: refuses to touch the EC on anything but the N6506MU.
- `install.sh` / `uninstall.sh`, systemd unit, menu entry.
- Optional `extras/dgpu-power`: cut the NVIDIA dGPU's power rail on battery.
- `NOTES.md`: the reverse-engineering notes on the EC fan tables.
