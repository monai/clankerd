.DEFAULT_GOAL := build
.PHONY: build clean

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
HOST_EXECUTABLES := $(addprefix $(CARGO_TARGET_DIR)/$(HOST_TARGET)/release/,vmctl clankerd-vmspawn clankerd-network)
GUEST_EXECUTABLE := $(CARGO_TARGET_DIR)/$(GUEST_TARGET)/release/clankerd-guestd
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

.PHONY: cargo-host cargo-guest
cargo-host: $(WORK)/host-libraries/.complete $(WORK)/configuration.json
	+PKG_CONFIG_PATH="$(realpath $(WORK)/host-libraries/pkgconfig)" PKG_CONFIG_LIBDIR="$(realpath $(WORK)/host-libraries/pkgconfig)" PKG_CONFIG_ALLOW_CROSS=1 PKG_CONFIG_SYSROOT_DIR=/ SDKROOT="$(HOST_SDK)" $(HOST_CARGO) --locked --release --target $(HOST_TARGET) -p vmctl -p clankerd-vmspawn -p clankerd-network

cargo-guest: $(WORK)/configuration.json
	+cargo build --locked --release --target $(GUEST_TARGET) --config 'target.$(GUEST_TARGET).linker="rust-lld"' -p clankerd-guestd

$(HOST_EXECUTABLES): cargo-host
	@test -f "$@"

$(GUEST_EXECUTABLE): cargo-guest
	@test -f "$@"

LICENSE_ARCHIVES := $(foreach input,$(LICENSE_INPUTS),$(DOWNLOADS)/$(input)-$($(input)_SHA256).archive)

$(WORK)/artifact/.complete: $(LICENSE_ARCHIVES) $(HOST_EXECUTABLES) $(GUEST_EXECUTABLE) $(WORK)/management/.complete $(WORK)/host-libraries/.complete $(INPUT_LOCK) build-support/artifact.mk scripts/verify-host-artifact.py crates/clankerd-vmspawn/entitlements.plist $(PHASE_TOOLS) FORCE
	+@$(ATOMIC) $(@D) $(filter-out FORCE,$^) -- $(MAKE) --no-print-directory -f build-support/artifact.mk STAGE='{out}' WORK="$(WORK)" INPUT_LOCK="$(INPUT_LOCK)" DOWNLOADS="$(DOWNLOADS)" HOST_TARGET="$(HOST_TARGET)" GUEST_TARGET="$(GUEST_TARGET)"

build: $(WORK)/artifact/.complete
	@$(PYTHON) scripts/publish-artifact.py $(WORK)/artifact $(RUST_OUT)

$(WORK)/test-libraries/.complete: $(WORK)/host-libraries/.complete $(INPUT_LOCK) build-support/artifact.mk $(PHASE_TOOLS) FORCE
	+@$(ATOMIC) $(@D) $(filter-out FORCE,$^) -- $(MAKE) --no-print-directory -f build-support/artifact.mk host-libraries-install STAGE='{out}' WORK="$(WORK)" INPUT_LOCK="$(INPUT_LOCK)" DOWNLOADS="$(DOWNLOADS)"

ifeq ($(BUILD_CONTEXT),Linux-aarch64)
$(WORK)/linux-test-tools/.complete: $(WORK)/management/.complete build-support/linux-test-tools/run-tool $(PHASE_TOOLS) FORCE
	@$(ATOMIC) $(@D) $(filter-out FORCE,$^) -- sh -ec 'mkdir -p "$$1"; install -m 755 build-support/linux-test-tools/run-tool "$$1/run-tool"; ln -s "$(abspath $(WORK)/management)" "$$1/management"; for tool in mke2fs e2fsck resize2fs debugfs; do ln -s run-tool "$$1/$$tool"; done' sh '{out}'
LINUX_TEST_PREREQUISITES := $(WORK)/linux-test-tools/.complete
LINUX_TEST_ENV := CLANKERD_TEST_TOOLS=$(abspath $(WORK)/linux-test-tools)
endif

include build-support/checks.mk

clean:
	rm -rf build target
