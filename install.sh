#!/bin/sh
# Install vfc, vfcd and vfc-gui system-wide. Build first as your user:
#   cargo build --release && pkexec "$PWD/install.sh"
set -eu
cd "$(dirname "$0")"
for b in vfc vfcd vfc-gui; do
	[ -x target/release/$b ] || { echo "target/release/$b missing: run cargo build --release first" >&2; exit 1; }
done
install -m 0755 target/release/vfc target/release/vfcd target/release/vfc-gui /usr/local/bin/
install -m 0644 packaging/vfcd.service /etc/systemd/system/vfcd.service
install -Dm 0644 packaging/vfc-gui.desktop /usr/local/share/applications/vfc-gui.desktop
echo acpi_call > /etc/modules-load.d/acpi_call.conf
# A vfcd started by hand (testing) would fight the service over the socket.
pkill -x vfcd && sleep 3 || true
systemctl daemon-reload
systemctl enable vfcd.service
systemctl restart vfcd.service
sleep 2
systemctl --no-pager --lines=5 status vfcd.service
