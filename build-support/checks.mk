# Developer checks are independent of artifact production. Only opt-in native
# VM tests require a signed bundle; ordinary tests and lint run Cargo directly.
CHECK_SYSTEM := $(shell uname -s)
RUST_COVERAGE_TOOLCHAIN ?= nightly-2026-10-08
export RUST_COVERAGE_TOOLCHAIN
.PHONY: test test-scripts test-rust test-linux test-native lint coverage coverage-setup

test: test-scripts test-rust

test-scripts:
	python3 -m unittest discover -s scripts/tests -p 'test_*.py'

ifeq ($(CHECK_SYSTEM),Darwin)
ifneq ($(origin HOST_RUSTFLAGS),undefined)
test-rust lint: export CARGO_ENCODED_RUSTFLAGS := $(HOST_RUSTFLAGS)
endif

test-rust:
	+cargo test --locked --workspace --exclude clankerd-guestd --lib --bins
	+cargo test --locked -p clankerd-proto --test spawn
	+cargo test --locked -p libkrun-sys --test errors
	+cargo test --locked -p vmctl --test version --test smoke_script --test native_bind_semantics

test-native: RUST_OUT := $(if $(OUT),$(OUT),$(RUST_OUT))
test-native: build
	+OUT="$(RUST_OUT)" cargo test --locked -p vmctl --test native_bind_semantics -- --ignored --test-threads=1

lint:
	+cargo fmt --all -- --check
	+cargo clippy --locked --workspace --exclude clankerd-guestd --all-targets -- -D warnings
else ifeq ($(CHECK_SYSTEM),Linux)
test-rust: test-linux

test-native:
	@echo 'Native VM tests require Apple Silicon macOS' >&2; exit 1

lint:
	+cargo fmt --all -- --check
	+cargo clippy --locked --workspace --all-targets -- -D warnings
else
test-rust test-native lint:
	@echo 'Developer checks require Linux or macOS' >&2; exit 1
endif

ifeq ($(CHECK_SYSTEM),Linux)
test-linux: $(LINUX_TEST_PREREQUISITES)
	+$(LINUX_TEST_ENV) cargo test --locked --workspace

coverage: coverage-setup $(LINUX_TEST_PREREQUISITES)
	$(LINUX_TEST_ENV) python3 scripts/rust-coverage.py

coverage-setup:
	rustup toolchain install "$(RUST_COVERAGE_TOOLCHAIN)" --profile minimal --component llvm-tools-preview
else
test-linux coverage coverage-setup:
	@echo 'Linux guest tests and coverage require a Linux execution context' >&2; exit 1
endif
