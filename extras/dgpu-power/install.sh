#!/bin/sh
# Optional: cut the NVIDIA dGPU's power rail on battery (N6506MU).
# The GPU is removed from the PCI bus on battery and rescanned on AC.
# Run as root:  pkexec "$PWD/install.sh"   Undo with:  install.sh --uninstall
set -eu
cd "$(dirname "$0")"
[ "$(id -u)" = 0 ] || { echo "run as root (sudo or pkexec)" >&2; exit 1; }
if [ "${1:-}" = --uninstall ]; then
	systemctl disable dgpu-power.service 2>/dev/null || true
	/usr/local/sbin/dgpu-power on 2>/dev/null || true
	rm -f /usr/local/sbin/dgpu-power /etc/udev/rules.d/90-dgpu-power.rules /etc/systemd/system/dgpu-power.service
	systemctl daemon-reload
	udevadm control --reload
	echo "dgpu-power removed"
	exit 0
fi
install -Dm 0755 dgpu-power /usr/local/sbin/dgpu-power
install -Dm 0644 90-dgpu-power.rules /etc/udev/rules.d/90-dgpu-power.rules
install -Dm 0644 dgpu-power.service /etc/systemd/system/dgpu-power.service
systemctl daemon-reload
udevadm control --reload
systemctl enable dgpu-power.service
systemctl restart dgpu-power.service || true
/usr/local/sbin/dgpu-power status
