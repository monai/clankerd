#!/usr/bin/env bash
set -Eeuo pipefail
work=${1:?Usage: m2-report-isolation-path.sh retained-directory}
[ -d "$work" ] || { echo "Missing directory: $work" >&2; exit 1; }
for file in "$work/forwarding.txt" "$work"/route-*.txt "$work/guest-a-routes.txt" "$work"/probes-*.jsonl; do
  [ -f "$file" ] || continue
  echo "$(basename "$file"):"
  cat "$file"
done
for capture in "$work"/*.pcap; do
  [ -f "$capture" ] || continue
  echo "$(basename "$capture") packet headers:"
  report=$(mktemp /tmp/clankerd-packet-report.XXXXXX)
  if sudo /usr/sbin/tcpdump -nn -e -v -r "$capture" > "$report" 2>&1; then
    head -40 "$report"
  else
    cat "$report"
    rm -f "$report"
    exit 1
  fi
  rm -f "$report"
done
