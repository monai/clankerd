#!/bin/sh
set -eu
label=io.clankerd.network
plist=/Library/LaunchDaemons/$label.plist
helper=/Library/PrivilegedHelperTools/clankerd-network
stop_helper() {
    if ! /bin/launchctl print "system/$label" >/dev/null 2>&1; then
        return 0
    fi
    /bin/launchctl bootout "system/$label"
    # bootout requests removal; launchd can retain the registration while the
    # old process exits. Do not bootstrap a replacement into that interval.
    remaining=15
    while /bin/launchctl print "system/$label" >/dev/null 2>&1; do
        if [ "$remaining" -eq 0 ]; then
            echo "$label is still registered after bootout" >&2
            return 1
        fi
        /bin/sleep 1
        remaining=$((remaining - 1))
    done
}
case "$1" in
    install)
        /usr/bin/plutil -lint "$3"
        stop_helper
        /usr/bin/install -d -o root -g wheel -m 755 /Library/PrivilegedHelperTools
        /usr/bin/install -o root -g wheel -m 755 "$2" "$helper.new"
        /bin/mv -f "$helper.new" "$helper"
        /usr/bin/install -o root -g wheel -m 644 "$3" "$plist"
        /bin/launchctl bootstrap system "$plist"
        ;;
    uninstall)
        stop_helper
        /bin/rm -f "$plist" "$helper"
        ;;
    restart)
        /bin/launchctl kickstart -k "system/$label"
        ;;
    *) exit 2 ;;
esac
