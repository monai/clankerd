#!/usr/bin/env bash
# Recheck the reported Mac failure with the user's confirmed library installation.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
export DYLD_LIBRARY_PATH="$(brew --prefix libkrunfw)/lib${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}"
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-L native=$(brew --prefix libkrun)/lib"
exec bash "$root/scripts/accept-native-build.sh" m2-smoke-lifetimes
