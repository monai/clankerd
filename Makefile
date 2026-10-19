.DEFAULT_GOAL := build
.PHONY: build test test-linux test-native lint coverage coverage-setup clean

# mise install supplies the pinned toolchain; all build phases run in one context.
# Binary distribution inputs and each assembly phase are real graph nodes.
BUILD_ROOT ?= build/self-contained
BUILD_CONTEXT := $(shell uname -s)-$(shell uname -m)
WORK := $(BUILD_ROOT)/$(BUILD_CONTEXT)
DOWNLOADS ?= $(BUILD_ROOT)/downloads
RUST_OUT ?= build/rust
CARGO_TARGET_DIR ?= $(abspath $(WORK)/cargo)
export CARGO_TARGET_DIR
ifneq ($(BUILD_JOBS),)
export CARGO_BUILD_JOBS := $(BUILD_JOBS)
endif
INPUT_LOCK ?= build-support/inputs.mk
include $(INPUT_LOCK)
PYTHON := python3
ATOMIC := $(PYTHON) scripts/atomic-directory.py
PHASE_TOOLS := scripts/atomic-directory.py scripts/artifact-tree.py Makefile
HOST_TARGET := aarch64-apple-darwin
GUEST_TARGET := aarch64-unknown-linux-musl
RUST_INPUTS := Cargo.toml Cargo.lock $(shell find crates -type f -name '*.rs' -o -name '*.toml' -o -name '*.tbd' -o -name '*.c' -o -name '*.h' -o -name '*.plist')
ifeq ($(shell uname -s),Darwin)
HOST_CARGO := cargo build
HOST_SDK := $(shell xcrun --sdk macosx --show-sdk-path)
else
HOST_CARGO := cargo zigbuild
HOST_SDK := $(abspath scripts/macos-sdk-stubs)
endif

.PHONY: FORCE verify-inputs
FORCE:

define binary_input
$(DOWNLOADS)/$(1)-$($(1)_SHA256).archive: FORCE scripts/binary-input.py
	@$(PYTHON) scripts/binary-input.py $$@ $$($(1)_SHA256) '$$($(1)_URL)'
$(WORK)/packages/$(1)/.complete: $(DOWNLOADS)/$(1)-$($(1)_SHA256).archive scripts/extract-binary.py $(PHASE_TOOLS) FORCE
	@$(ATOMIC) $$(@D) $(DOWNLOADS)/$(1)-$($(1)_SHA256).archive scripts/extract-binary.py $(PHASE_TOOLS) -- $(PYTHON) scripts/extract-binary.py $(DOWNLOADS)/$(1)-$($(1)_SHA256).archive '{out}'
endef
$(foreach input,$(INPUTS),$(eval $(call binary_input,$(input))))
verify-inputs: $(foreach input,$(INPUTS),$(DOWNLOADS)/$(input)-$($(input)_SHA256).archive)
GUEST_PACKAGES := $(addprefix $(WORK)/packages/,$(GUEST_INPUTS))
HOST_PACKAGES := $(addprefix $(WORK)/packages/,$(HOST_INPUTS))
MANAGEMENT_ROOTFS := build-support/management-rootfs
MANAGEMENT_FILES := $(shell find $(MANAGEMENT_ROOTFS))

$(WORK)/management/.complete: $(addsuffix /.complete,$(GUEST_PACKAGES)) $(MANAGEMENT_FILES) scripts/assemble-management.py $(PHASE_TOOLS) FORCE
	@$(ATOMIC) $(@D) $(filter-out FORCE,$^) -- $(PYTHON) scripts/assemble-management.py '{out}' $(MANAGEMENT_ROOTFS) $(GUEST_PACKAGES) --required $(GUEST_REQUIRED)

$(WORK)/host-libraries/.complete: $(addsuffix /.complete,$(HOST_PACKAGES)) scripts/relocate-host.py $(PHASE_TOOLS) FORCE
	@$(ATOMIC) $(@D) $(filter-out FORCE,$^) -- $(PYTHON) scripts/relocate-host.py '{out}' $(HOST_PACKAGES) --required $(HOST_REQUIRED)

$(WORK)/configuration.json: scripts/build-configuration.py FORCE
	@$(PYTHON) scripts/build-configuration.py $@ "$(HOST_SDK)" "$(CARGO_TARGET_DIR)"

RUST_HOST_INPUTS := $(filter-out crates/clankerd-guestd/%,$(RUST_INPUTS))
RUST_GUEST_INPUTS := $(filter Cargo.toml Cargo.lock crates/clankerd-guestd/% crates/clankerd-proto/%,$(RUST_INPUTS))
$(WORK)/host-inputs.json: $(RUST_HOST_INPUTS) scripts/record-build-inputs.py FORCE
	@$(PYTHON) scripts/record-build-inputs.py $@ $(RUST_HOST_INPUTS)
$(WORK)/guest-inputs.json: $(RUST_GUEST_INPUTS) scripts/record-build-inputs.py FORCE
	@$(PYTHON) scripts/record-build-inputs.py $@ $(RUST_GUEST_INPUTS)

$(WORK)/host-rust/.complete: $(WORK)/host-libraries/.complete $(WORK)/configuration.json $(WORK)/host-inputs.json $(RUST_HOST_INPUTS) $(PHASE_TOOLS) FORCE
	@$(ATOMIC) $(@D) $(filter-out FORCE,$^) -- sh -ec 'PKG_CONFIG_PATH="$(realpath $(WORK)/host-libraries/pkgconfig)" PKG_CONFIG_LIBDIR="$(realpath $(WORK)/host-libraries/pkgconfig)" PKG_CONFIG_ALLOW_CROSS=1 PKG_CONFIG_SYSROOT_DIR=/ SDKROOT="$(HOST_SDK)" $(HOST_CARGO) --locked --release --target $(HOST_TARGET) -p vmctl -p clankerd-vmspawn -p clankerd-network; mkdir -p "$$1"; cp "$(CARGO_TARGET_DIR)/$(HOST_TARGET)/release/vmctl" "$(CARGO_TARGET_DIR)/$(HOST_TARGET)/release/clankerd-vmspawn" "$(CARGO_TARGET_DIR)/$(HOST_TARGET)/release/clankerd-network" "$$1/"' sh '{out}'

$(WORK)/guest-rust/.complete: $(WORK)/configuration.json $(WORK)/guest-inputs.json $(RUST_GUEST_INPUTS) $(PHASE_TOOLS) FORCE
	@$(ATOMIC) $(@D) $(filter-out FORCE,$^) -- sh -ec 'cargo build --locked --release --target $(GUEST_TARGET) --config target.$(GUEST_TARGET).linker=\"rust-lld\" -p clankerd-guestd; mkdir -p "$$1"; cp "$(CARGO_TARGET_DIR)/$(GUEST_TARGET)/release/clankerd-guestd" "$$1/"' sh '{out}'

LICENSE_ARCHIVES := $(foreach input,$(LICENSE_INPUTS),$(DOWNLOADS)/$(input)-$($(input)_SHA256).archive)

$(WORK)/artifact/.complete: $(LICENSE_ARCHIVES) $(WORK)/host-rust/.complete $(WORK)/guest-rust/.complete $(WORK)/management/.complete $(WORK)/host-libraries/.complete $(INPUT_LOCK) scripts/assemble-artifact.py scripts/verify-host-artifact.py crates/clankerd-vmspawn/entitlements.plist $(PHASE_TOOLS) FORCE
	@$(ATOMIC) $(@D) $(filter-out FORCE,$^) -- $(PYTHON) scripts/assemble-artifact.py '{out}' $(WORK) $(INPUT_LOCK) $(foreach input,$(LICENSE_INPUTS),$(input)=$(DOWNLOADS)/$(input)-$($(input)_SHA256).archive)

build: $(WORK)/artifact/.complete
	@$(PYTHON) scripts/publish-artifact.py $(WORK)/artifact $(RUST_OUT)

ifeq ($(BUILD_CONTEXT),Linux-aarch64)
$(WORK)/linux-test-tools/.complete: $(WORK)/management/.complete build-support/linux-test-tools/run-tool $(PHASE_TOOLS) FORCE
	@$(ATOMIC) $(@D) $(filter-out FORCE,$^) -- sh -ec 'mkdir -p "$$1"; install -m 755 build-support/linux-test-tools/run-tool "$$1/run-tool"; ln -s "$(abspath $(WORK)/management)" "$$1/management"; for tool in mke2fs e2fsck resize2fs debugfs; do ln -s run-tool "$$1/$$tool"; done' sh '{out}'
TEST_PREREQUISITES := $(WORK)/linux-test-tools/.complete
TEST_ENV := CLANKERD_TEST_TOOLS=$(abspath $(WORK)/linux-test-tools)
endif

ifeq ($(shell uname -s),Darwin)
TEST_PREREQUISITES := build
endif

test: $(TEST_PREREQUISITES)
	python3 -m unittest discover -s scripts/tests -p 'test_*.py'
	$(TEST_ENV) python3 scripts/rust-test.py test

test-linux:
	python3 scripts/rust-test.py test-linux

test-native:
	python3 scripts/rust-test.py test-native

lint:
	python3 scripts/rust-test.py lint

coverage: coverage-setup
	python3 scripts/rust-test.py coverage

coverage-setup:
	python3 scripts/rust-test.py coverage-setup

clean:
	rm -rf build target
