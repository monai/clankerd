#!/bin/sh
set -eu
label=io.clankerd.network
plist=/Library/LaunchDaemons/$label.plist
helper=/Library/PrivilegedHelperTools/clankerd-network
case "$1" in
    install)
        /usr/bin/plutil -lint "$3"
        /bin/launchctl bootout "system/$label" 2>/dev/null || :
        /usr/bin/install -d -o root -g wheel -m 755 /Library/PrivilegedHelperTools
        /usr/bin/install -o root -g wheel -m 755 "$2" "$helper.new"
        /bin/mv -f "$helper.new" "$helper"
        /usr/bin/install -o root -g wheel -m 644 "$3" "$plist"
        /bin/launchctl bootstrap system "$plist"
        ;;
    uninstall)
        /bin/launchctl bootout "system/$label" 2>/dev/null || :
        /bin/rm -f "$plist" "$helper"
        ;;
    restart)
        /bin/launchctl kickstart -k "system/$label"
        ;;
    *) exit 2 ;;
esac
