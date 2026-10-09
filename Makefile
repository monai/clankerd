.DEFAULT_GOAL := build
.PHONY: build test lint coverage coverage-setup clean rust-build rust-sign rust-verify rust-test rust-lint rust rust-coverage rust-coverage-setup

build: rust
test: rust-test
lint: rust-lint
coverage: rust-coverage
coverage-setup: rust-coverage-setup

# Rust workspace. Prerequisite: `mise install`. One command: `make rust`
# (build + sign + verify); outputs land in build/rust/<os-arch>/.
RUST_OUT := build/rust
TARGET_DIR := $(or $(CARGO_TARGET_DIR),target)
DARWIN := aarch64-apple-darwin
MUSL := aarch64-unknown-linux-musl
ifeq ($(shell uname -s),Darwin)
DARWIN_SDKROOT = $$(xcrun --sdk macosx --show-sdk-path)
else
DARWIN_SDKROOT = $(CURDIR)/scripts/macos-sdk-stubs
endif

rust-lint:
	cargo fmt --check
	cargo clippy --workspace --all-targets -- -D warnings

rust-test:
	cargo test --workspace

# Branch coverage requires nightly; production builds retain the pinned stable compiler.
RUST_COVERAGE_TOOLCHAIN := nightly-2026-10-08
COVERAGE_OUT ?= build/coverage
COVERAGE_JOBS ?= 2

rust-coverage-setup:
	rustup toolchain install $(RUST_COVERAGE_TOOLCHAIN) --profile minimal --component llvm-tools-preview

rust-coverage:
	mkdir -p $(COVERAGE_OUT)
	CARGO_TARGET_DIR="$(TARGET_DIR)/llvm-cov-target" CARGO_LLVM_COV_TARGET_DIR="$(TARGET_DIR)/llvm-cov-target" \
	  CARGO_BUILD_JOBS="$(COVERAGE_JOBS)" LLVM_PROFILE_FILE_NAME="clankerd-%p-%m%c.profraw" \
	  RUSTFLAGS="$(RUSTFLAGS) -Cllvm-args=-runtime-counter-relocation" \
	  cargo +$(RUST_COVERAGE_TOOLCHAIN) llvm-cov --workspace --branch -j $(COVERAGE_JOBS) --json --output-path $(COVERAGE_OUT)/coverage.json
	cargo +$(RUST_COVERAGE_TOOLCHAIN) llvm-cov report
	cargo +$(RUST_COVERAGE_TOOLCHAIN) llvm-cov report --html --output-dir $(COVERAGE_OUT)

# vmctl links Security.framework and CoreFoundation (TLS certificate
# verification via rustls-platform-verifier). Linux has no macOS SDK, so the
# darwin link uses an SDK root holding link-time stubs of just the symbols we
# import (scripts/macos-sdk-stubs); dyld binds them to the real frameworks.
# On macOS, host build scripts need the complete Apple SDK, including libSystem.
rust-build:
	sdkroot="$(DARWIN_SDKROOT)" && SDKROOT="$$sdkroot" cargo zigbuild --release --target $(DARWIN) -p vmctl -p clankerd-vmspawn
	cargo build --release --target $(MUSL) --config 'target.$(MUSL).linker="rust-lld"' -p clankerd-guestd
	mkdir -p $(RUST_OUT)/darwin-arm64 $(RUST_OUT)/linux-arm64
	cp $(TARGET_DIR)/$(DARWIN)/release/vmctl $(TARGET_DIR)/$(DARWIN)/release/clankerd-vmspawn $(RUST_OUT)/darwin-arm64/
	cp $(TARGET_DIR)/$(MUSL)/release/clankerd-guestd $(RUST_OUT)/linux-arm64/
	scripts/build-e2fsprogs.sh $(RUST_OUT)/linux-arm64

# Ad-hoc signs with the hypervisor entitlement (rcodesign works on Linux).
rust-sign:
	rcodesign sign -e crates/clankerd-vmspawn/entitlements.plist \
	  --binary-identifier io.clankerd.vmspawn $(RUST_OUT)/darwin-arm64/clankerd-vmspawn
	rcodesign sign --binary-identifier io.clankerd.vmctl $(RUST_OUT)/darwin-arm64/vmctl

rust-verify:
	scripts/rust-verify.sh $(RUST_OUT)

rust: rust-build rust-sign rust-verify

clean:
	rm -rf build target
