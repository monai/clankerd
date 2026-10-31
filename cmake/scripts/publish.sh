#!/usr/bin/env bash
set -euo pipefail
stage=$1
publish=$2
if [ -e "$publish" ] && [ ! -L "$publish" ]; then
  echo 'Publication path must be absent or a bundle symlink' >&2
  exit 1
fi
if [ -L "$publish" ]; then
  case "$(readlink "$publish")" in "$(basename "$publish").bundles/bundle."*) ;; *) echo 'Publication path points outside its bundle directory' >&2; exit 1;; esac
fi
if [ -L "$publish" ]; then
  status=0
  "${3:-python3}" "$(dirname "$0")/compare-trees.py" "$stage" "$publish" || status=$?
  if [ "$status" = 0 ]; then exit 0; fi
  if [ "$status" != 1 ]; then exit "$status"; fi
fi
mkdir -p "$publish.bundles"
bundle=$(mktemp -d "$publish.bundles/bundle.XXXXXX")
link="$publish.${bundle##*/}.link"
target="$(basename "$publish").bundles/${bundle##*/}"
trap 'rm -f "$link"; if [ "$(readlink "$publish" 2>/dev/null || :)" != "$target" ]; then rm -rf "$bundle"; fi' EXIT
trap 'exit 1' HUP INT TERM
cp -pRP "$stage/." "$bundle/"
ln -s "$target" "$link"
if [ "$(uname -s)" = Darwin ]; then mv -fh "$link" "$publish"; else mv -fT "$link" "$publish"; fi
