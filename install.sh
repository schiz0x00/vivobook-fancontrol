#!/bin/sh
# Install vivobook-fancontrol system-wide.
#
#   From a release:   pkexec "$PWD/install.sh"        (or: sudo ./install.sh)
#   From source:      cargo build --release && pkexec "$PWD/install.sh"
#
# Installs vfc, vfcd, vfc-gui and fan-control to /usr/local/bin, the vfcd
# systemd service and the "Fan Control" menu entry. Undo with uninstall.sh.
set -eu
cd "$(dirname "$0")"

die() { echo "install.sh: $*" >&2; exit 1; }

[ "$(id -u)" = 0 ] || die "run as root: pkexec \"\$PWD/install.sh\" or sudo ./install.sh"

board=$(cat /sys/class/dmi/id/board_name 2>/dev/null || true)
if [ "$board" != N6506MU ] && [ "${VFC_FORCE_UNSUPPORTED:-}" != 1 ]; then
	die "this is a ${board:-unknown} board; only the ASUS Vivobook Pro 15 N6506MU is supported.
Its EC addresses mean something else on other laptops. See README.md."
fi

if ! modinfo acpi_call >/dev/null 2>&1; then
	die "the acpi_call kernel module is missing. Install it first:
  Debian/Ubuntu: apt install acpi-call-dkms
  Others:        build it from https://github.com/nix-community/acpi_call
  Arch:          pacman -S acpi_call-dkms
With Secure Boot on, DKMS must sign it with an enrolled MOK (see README.md)."
fi

if [ -x bin/vfcd ]; then bindir=bin
elif [ -x target/release/vfcd ]; then bindir=target/release
else die "no binaries found: run 'cargo build --release' first"
fi
for b in vfc vfcd vfc-gui fan-control; do
	[ -x "$bindir/$b" ] || die "$bindir/$b missing"
done

modprobe acpi_call || die "modprobe acpi_call failed (unsigned module with Secure Boot on?)"
echo acpi_call > /etc/modules-load.d/acpi_call.conf

for b in vfc vfcd vfc-gui fan-control; do
	install -m 0755 "$bindir/$b" /usr/local/bin/
done
install -Dm 0644 packaging/vfcd.service /etc/systemd/system/vfcd.service
install -Dm 0644 packaging/fan-control.desktop /usr/local/share/applications/fan-control.desktop
install -Dm 0644 packaging/vfc-gui.desktop /usr/local/share/applications/vfc-gui.desktop
command -v update-desktop-database >/dev/null && update-desktop-database -q /usr/local/share/applications || true

# A vfcd started by hand (testing) would fight the service over the socket.
if pgrep -x vfcd >/dev/null && ! systemctl is-active -q vfcd.service; then
	pkill -x vfcd || true
	sleep 3
fi
systemctl daemon-reload
systemctl enable vfcd.service
systemctl restart vfcd.service
sleep 2
if systemctl is-active -q vfcd.service; then
	echo
	echo "Installed. Open \"Fan Control\" from the app menu."
	group=$(stat -c %G /run/vfcd.sock 2>/dev/null || echo sudo)
	user=${SUDO_USER:-$(id -nu "${PKEXEC_UID:-0}" 2>/dev/null || echo)}
	if [ -n "$user" ] && [ "$user" != root ] && ! id -nG "$user" | tr ' ' '\n' | grep -qx "$group"; then
		echo "Note: $user is not in the '$group' group, which the app needs: usermod -aG $group $user"
	fi
else
	systemctl --no-pager --lines=10 status vfcd.service || true
	die "vfcd did not start (see above)"
fi
