#!/usr/bin/env bash
# Install the reviewed prefix extension separately from the upstream helper.
set -Eeuo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'Requires the Mac host' >&2; exit 1; }
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d /tmp/clankerd-helper-install.XXXXXX)
trap 'echo "Helper build evidence: $work"' EXIT
sudo -n true
/usr/bin/python3 -I -c 'import fcntl, ipaddress, json'
user=$(id -un)
case "$user" in ''|*[!A-Za-z0-9._-]*) echo 'Unsupported sudoers user spelling' >&2; exit 1;; esac
"$root/scripts/build-vmnet-helper.sh" "$work/build" > "$work/build.log" 2>&1 || { tail -40 "$work/build.log"; exit 1; }
printf '%s ALL=(root) NOPASSWD: /opt/clankerd/libexec/vmnet-helper, /opt/clankerd/libexec/clankerd-network-pf\n' "$user" > "$work/sudoers"
sudo -n install -o root -g wheel -m 0440 "$work/sudoers" "$work/sudoers.checked"
sudo -n /usr/sbin/visudo -cf "$work/sudoers.checked"
for path in /opt/clankerd /opt/clankerd/libexec /opt/clankerd/libexec/vmnet-helper /opt/clankerd/libexec/clankerd-network-pf /opt/clankerd/share /opt/clankerd/share/licenses /opt/clankerd/share/licenses/vmnet-helper.txt /etc/sudoers.d/clankerd-vmnet-helper; do
  [ ! -L "$path" ] || { echo "Refusing to replace symlink: $path" >&2; exit 1; }
done
echo 'Installing root-owned vmnet helper, network PF controller and dedicated sudoers rule'
sudo -n install -d -o root -g wheel -m 0755 /opt/clankerd /opt/clankerd/libexec
sudo -n install -o root -g wheel -m 0755 "$work/build/vmnet-helper" /opt/clankerd/libexec/vmnet-helper
sudo -n install -o root -g wheel -m 0755 "$root/scripts/clankerd-network-pf.py" /opt/clankerd/libexec/clankerd-network-pf
sudo -n install -d -o root -g wheel -m 0755 /opt/clankerd/share/licenses
sudo -n install -o root -g wheel -m 0644 "$work/build/source/LICENSES/Apache-2.0.txt" /opt/clankerd/share/licenses/vmnet-helper.txt
sudo -n install -o root -g wheel -m 0440 "$work/sudoers.checked" /etc/sudoers.d/clankerd-vmnet-helper
cmp "$work/build/vmnet-helper" /opt/clankerd/libexec/vmnet-helper
cmp "$root/scripts/clankerd-network-pf.py" /opt/clankerd/libexec/clankerd-network-pf
export CLANKERD_VMNET_HELPER=/opt/clankerd/libexec/vmnet-helper
sudo -n "$CLANKERD_VMNET_HELPER" --version
"$root/scripts/m2-run-network-gates.sh" "$@"
