#!/usr/bin/env bash
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
echo 'Installed VMM executables:'
for name in qemu-system-aarch64 vfkit limactl tart prlctl VBoxManage; do
  if path=$(command -v "$name"); then echo "$name: $path"; else echo "$name: absent from PATH"; fi
done
echo 'Installed VMM applications:'
for app in /Applications/UTM.app /Applications/Parallels\ Desktop.app /Applications/VMware\ Fusion.app /Applications/VirtualBox.app; do
  if [ -d "$app" ]; then echo "$app: present"; fi
done
echo 'Native helper:'
/opt/vmnet-helper/bin/vmnet-helper --version
echo 'Physical interface inventory:'
/usr/sbin/networksetup -listallhardwareports
echo 'Active interface addresses and media:'
ifconfig | awk '/^[a-zA-Z0-9]+:/ {print} /^[[:space:]]+(inet |inet6 |media:|status:)/ {print}'
echo 'Current forwarding and bridge-filter controls:'
for key in net.inet.ip.forwarding net.inet6.ip6.forwarding net.link.bridge.pfil_member net.link.bridge.pfil_bridge; do
  sysctl "$key" 2>&1 || true
done
echo 'IPv6 default route:'
route -n get -inet6 default 2>&1 || true
echo 'This read-only inventory establishes available test tools/interfaces, not another-VMM or independent LAN-peer acceptance.'
