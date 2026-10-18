#!/usr/bin/env bash
# One native entry point: build, relocate, inspect, and exercise the artifact.
set -Eeuo pipefail
# Bash can otherwise read later lines from a checkout edited during a long run.
if [ "${1:-}" != --acceptance-script-snapshot ]; then
  exec bash -c "$(cat "$0")" "$0" --acceptance-script-snapshot "$@"
fi
shift
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
[ "$(uname -sm)" = 'Darwin arm64' ] || { echo 'Requires Apple Silicon macOS' >&2; exit 1; }
[ "$(sw_vers -productVersion | cut -d. -f1)" = 15 ] || { echo 'The acceptance baseline requires macOS 15' >&2; exit 1; }
python3 -c 'import sys; assert sys.version_info >= (3,12), "Python 3.12+ required"'
evidence=${EVIDENCE_DIR:-$root/build/acceptance-$(date -u +%Y%m%dT%H%M%SZ)}
mkdir -p "$evidence"
evidence=$(cd "$evidence" && pwd)
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$root/build/acceptance-cargo-Darwin-arm64}
# Runtime checks must inherit no dependency paths or earlier experiment overlays.
for variable in $(env | cut -d= -f1); do
  case "$variable" in DYLD_*|CONTROL_PLANE_*|CLANKERD_VMNET_HELPER|CLANKERD_NETWORK|CLANKERD_VMSPAWN|CLANKERD_GUESTD|CLANKERD_NETWORK_PF|CLANKERD_BOOT_DIR|CLANKERD_BOOT_DIR_ROOT|CLANKERD_DEV_GUESTD|CLANKERD_VMSPAWN_DEV_LOCAL|VMNET_TEST_LIBRARY_PATH) unset "$variable";; esac
done
printf 'Evidence: %s\n' "$evidence"
exec 3>&1
if [ "${ACCEPTANCE_VERBOSE:-0}" = 1 ]; then
  exec > >(tee "$evidence/acceptance.log") 2>&1
else
  exec > >(tee "$evidence/acceptance.log" | python3 scripts/summarize-self-contained-build.py --progress) 2>&1
fi
finish() {
  status=$?
  if [ -n "${refresh_pid:-}" ]; then kill "$refresh_pid" 2>/dev/null || true; fi
  python3 - "$evidence" "$status" <<'PYRESULT' || true
import json, sys
from pathlib import Path
root=Path(sys.argv[1]); code=int(sys.argv[2]); results=root/'results.tsv'
(root/'result.json').write_text(json.dumps({'status':'PASS' if code==0 else 'FAIL', 'exit_code':code,
    'gates':results.read_text().splitlines() if results.exists() else [],
    'management':json.loads((root/'management-result.json').read_text()) if (root/'management-result.json').exists() else None}, indent=2)+'\n')
PYRESULT
  python3 scripts/summarize-self-contained-build.py "$evidence" >&3 || true
  echo "Evidence retained: $evidence"
  exit "$status"
}
trap finish EXIT
stages=(m2-boot-check m2-smoke-command-system m2-smoke-native-prefixes m2-smoke-lifetimes m2-smoke-recovery m2-smoke-test)
if [ "$#" -gt 0 ]; then stages=("$@"); fi
step_number=0
step_total=$((9 + ${#stages[@]}))
step() {
  step_number=$((step_number + 1))
  printf '\n[%d/%d] %s\n' "$step_number" "$step_total" "$*"
}
step 'Host and source evidence'
sw_vers
git rev-parse HEAD > "$evidence/commit.txt"
git diff HEAD -- crates scripts Makefile build-support > "$evidence/worktree.diff"
# Parse and link the stub with Apple's linker before the expensive Cargo build.
printf 'extern int krun_create_ctx(void); int main(void) { return krun_create_ctx(); }\n' \
  | xcrun clang -arch arm64 -x c - -L crates/libkrun-sys/stubs -lkrun \
      -o "$evidence/libkrun-stub-check"
step 'Script regressions (usually under 30 seconds)'
python3 -m unittest scripts.tests.test_build_graph scripts.tests.test_accept_vm_process scripts.tests.test_acceptance_gates
step 'Build artifact (long-running: about 1–10 minutes; cold downloads may take longer)'
make -j"${BUILD_JOBS:-4}" build
step 'Copy and verify relocated artifact'
python3 scripts/artifact-tree.py check build/rust
cp -R build/rust/. "$evidence/bundle"
export OUT=$evidence/bundle
python3 scripts/artifact-tree.py check "$OUT"
# A warm build must keep both the generation and every completed file unchanged.
step 'Verify unchanged warm build'
cp build/rust/.complete "$evidence/before-warm.json"
make -j"${BUILD_JOBS:-4}" build
cmp build/rust/.complete "$evidence/before-warm.json"
step 'Apple SDK, signatures, and host library audit'
# Compile the owned native transport against Apple's real headers too.
xcrun clang -fblocks -Wall -Wextra -Werror -mmacosx-version-min=15.0 -c \
  crates/clankerd-network/src/vmnet.c -o "$evidence/vmnet-sdk-check.o"
for binary in "$OUT"/darwin-arm64/{vmctl,clankerd-vmspawn,clankerd-network} "$OUT"/darwin-arm64/lib/*.dylib; do
  codesign --verify --strict "$binary"
  otool -L "$binary"
done
python3 scripts/verify-host-artifact.py "$OUT"
"$OUT/darwin-arm64/vmctl" --version
step 'Build guest probe (usually under 1 minute)'
# The test probe belongs to acceptance, never to the published runtime bundle.
cargo build --locked --release --target aarch64-unknown-linux-musl \
  --config 'target.aarch64-unknown-linux-musl.linker="rust-lld"' \
  -p clankerd-guestd --example workload-probe
step 'Authorization and management isolation (long-running: about 2–5 minutes; password required)'
# A private executable path cannot rely on an old helper-specific sudoers rule.
mkdir "$evidence/private-authorization"
cp "$OUT/darwin-arm64/clankerd-network" "$evidence/private-authorization/clankerd-network"
if sudo -k -n -- "$evidence/private-authorization/clankerd-network" authorization-session > "$evidence/authorization-denied.log" 2>&1; then
  echo 'Cannot prove declined authorization: this account has passwordless authorization for the private executable.' >&2
  exit 1
fi
# First-use authentication occurs through vmctl itself, without a warmed
# credential cache. Management checks also revoke that cache before recovery.
python3 scripts/accept-management.py "$OUT" \
  "$CARGO_TARGET_DIR/aarch64-unknown-linux-musl/release/examples/workload-probe" "$evidence"
sudo -v
# Legacy gates use sudo for read-only PF observations and temporary test fixtures.
( while sleep 30; do sudo -n -v || exit; done ) &
refresh_pid=$!
step 'Native bind semantics (long-running: about 1–5 minutes)'
cargo test --locked -p vmctl --test native_bind_semantics -- --ignored --test-threads=1 \
  > "$evidence/native-bind-semantics.log" 2>&1
: > "$evidence/results.tsv"
failures=0
for stage in "${stages[@]}"; do
  case "$stage" in m2-boot-check|m2-smoke-command-system|m2-smoke-native-prefixes|m2-smoke-lifetimes|m2-smoke-recovery|m2-smoke-test) ;; *) echo "Unsupported gate: $stage"; exit 1;; esac
  case "$stage" in
    m2-boot-check) estimate='2–10 minutes, including image/volume/network boots';;
    m2-smoke-command-system) estimate='1–5 minutes';;
    m2-smoke-native-prefixes) estimate='5–15 minutes, including packet timeouts and recovery';;
    m2-smoke-lifetimes) estimate='3–8 minutes, deliberately waiting for lease/RA expiry';;
    m2-smoke-recovery) estimate='3–8 minutes, including deliberate network failures';;
    m2-smoke-test) estimate='5–20 minutes, including Docker/image downloads';;
  esac
  step "RUN $stage (long-running: about $estimate; log: $evidence/$stage.log)"
  if python3 scripts/run-acceptance-gate.py "$evidence/$stage.log" bash "scripts/$stage.sh"; then
    printf 'PASS\t%s\n' "$stage" | tee -a "$evidence/results.tsv"
  else
    failures=$((failures+1))
    printf 'FAIL\t%s\n' "$stage" | tee -a "$evidence/results.tsv"
    tail -40 "$evidence/$stage.log"
  fi
done
printf '%s\n' 'Sleep/wake and external IPv6 remain unverified unless explicitly observed in gate evidence.'
[ "$failures" = 0 ]
