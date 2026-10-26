#!/bin/sh
# Remove vivobook-fancontrol. The fans go back to the firmware's own
# tables when vfcd stops. Keeps /etc/vfc unless --purge is given.
set -eu
[ "$(id -u)" = 0 ] || { echo "run as root: pkexec \"\$PWD/uninstall.sh\" or sudo ./uninstall.sh" >&2; exit 1; }
systemctl disable --now vfcd.service 2>/dev/null || true
rm -f /etc/systemd/system/vfcd.service /etc/modules-load.d/acpi_call.conf
rm -f /usr/local/bin/vfc /usr/local/bin/vfcd /usr/local/bin/vfc-gui /usr/local/bin/fan-control
rm -f /usr/local/share/applications/fan-control.desktop /usr/local/share/applications/vfc-gui.desktop
systemctl daemon-reload
[ "${1:-}" = --purge ] && rm -rf /etc/vfc
echo "vivobook-fancontrol removed."
