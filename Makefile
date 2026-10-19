.DEFAULT_GOAL := build
.PHONY: build clean

# mise install supplies the pinned toolchain; all build phases run in one context.
# Binary distribution inputs and each assembly phase are real graph nodes.
BUILD_ROOT ?= build/work
BUILD_CONTEXT := $(shell uname -s)-$(shell uname -m)
WORK := $(BUILD_ROOT)/$(BUILD_CONTEXT)
DOWNLOADS ?= $(BUILD_ROOT)/downloads
RUST_OUT ?= build/artifact
CARGO_TARGET_DIR ?= $(abspath $(WORK)/cargo)
export CARGO_TARGET_DIR
ifneq ($(BUILD_JOBS),)
export CARGO_BUILD_JOBS := $(BUILD_JOBS)
endif
INPUT_LOCK ?= build-support/inputs.mk
include $(INPUT_LOCK)
PYTHON := python3
.DELETE_ON_ERROR:
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

.PHONY: verify-inputs

define binary_input
$(DOWNLOADS)/$(1)-$($(1)_SHA256).archive:
	mkdir -p "$$(@D)"
	curl --fail --location --retry 3 $(if $(findstring https://ghcr.io/,$($(1)_URL)),-H 'Authorization: Bearer QQ==') --output "$$@.tmp" '$($(1)_URL)'
	printf '%s  %s\n' '$($(1)_SHA256)' "$$@.tmp" | shasum -a 256 -c -
	mv -f "$$@.tmp" "$$@"
$(WORK)/packages/$(1)/.stamp: $(DOWNLOADS)/$(1)-$($(1)_SHA256).archive scripts/extract-binary.py Makefile
	printf '%s  %s\n' '$($(1)_SHA256)' "$$<" | shasum -a 256 -c -
	rm -rf "$$(@D)"
	$(PYTHON) scripts/extract-binary.py "$$<" "$$(@D)"
	touch "$$@"
endef
$(foreach input,$(INPUTS),$(eval $(call binary_input,$(input))))
verify-inputs: $(foreach input,$(INPUTS),$(DOWNLOADS)/$(input)-$($(input)_SHA256).archive)
	$(foreach input,$(INPUTS),printf '%s  %s\n' '$($(input)_SHA256)' '$(DOWNLOADS)/$(input)-$($(input)_SHA256).archive' | shasum -a 256 -c - && ) true
GUEST_PACKAGES := $(addprefix $(WORK)/packages/,$(GUEST_INPUTS))
HOST_PACKAGES := $(addprefix $(WORK)/packages/,$(HOST_INPUTS))
MANAGEMENT_ROOTFS := build-support/management-rootfs
MANAGEMENT_FILES := $(shell find $(MANAGEMENT_ROOTFS))

$(WORK)/management/.stamp: $(addsuffix /.stamp,$(GUEST_PACKAGES)) $(MANAGEMENT_FILES) $(INPUT_LOCK) scripts/assemble-management.py Makefile
	rm -rf "$(@D)"
	$(PYTHON) scripts/assemble-management.py "$(@D)" $(MANAGEMENT_ROOTFS) $(GUEST_PACKAGES) --required $(GUEST_REQUIRED)
	touch "$@"

$(WORK)/host-libraries/.stamp: $(addsuffix /.stamp,$(HOST_PACKAGES)) $(INPUT_LOCK) scripts/relocate-host.py Makefile
	rm -rf "$(@D)"
	$(PYTHON) scripts/relocate-host.py "$(@D)" $(HOST_PACKAGES) --required $(HOST_REQUIRED)
	touch "$@"

.PHONY: cargo-host cargo-guest
cargo-host: $(WORK)/host-libraries/.stamp
	+PKG_CONFIG_PATH="$(abspath $(WORK)/host-libraries/pkgconfig)" PKG_CONFIG_LIBDIR="$(abspath $(WORK)/host-libraries/pkgconfig)" PKG_CONFIG_ALLOW_CROSS=1 PKG_CONFIG_SYSROOT_DIR=/ SDKROOT="$(HOST_SDK)" $(HOST_CARGO) --locked --release --target $(HOST_TARGET) -p vmctl -p clankerd-vmspawn -p clankerd-network

cargo-guest:
	+cargo build --locked --release --target $(GUEST_TARGET) --config 'target.$(GUEST_TARGET).linker="rust-lld"' -p clankerd-guestd

$(HOST_EXECUTABLES): cargo-host
	@test -f "$@"

$(GUEST_EXECUTABLE): cargo-guest
	@test -f "$@"

LICENSE_ARCHIVES := $(foreach input,$(LICENSE_INPUTS),$(DOWNLOADS)/$(input)-$($(input)_SHA256).archive)

build: $(LICENSE_ARCHIVES) $(HOST_EXECUTABLES) $(GUEST_EXECUTABLE) $(WORK)/management/.stamp $(WORK)/host-libraries/.stamp
	+$(MAKE) --no-print-directory -f build-support/artifact.mk STAGE="$(RUST_OUT)" WORK="$(WORK)" INPUT_LOCK="$(INPUT_LOCK)" DOWNLOADS="$(DOWNLOADS)" HOST_TARGET="$(HOST_TARGET)" GUEST_TARGET="$(GUEST_TARGET)"

.PHONY: test-libraries
test-libraries: $(WORK)/host-libraries/.stamp
	+$(MAKE) --no-print-directory -f build-support/artifact.mk host-libraries-install STAGE="$(WORK)/test-libraries" WORK="$(WORK)" INPUT_LOCK="$(INPUT_LOCK)" DOWNLOADS="$(DOWNLOADS)"

ifeq ($(BUILD_CONTEXT),Linux-aarch64)
LINUX_TOOLS := $(WORK)/linux-test-tools
$(LINUX_TOOLS):
	mkdir -p "$@"

$(LINUX_TOOLS)/run-tool: build-support/linux-test-tools/run-tool | $(LINUX_TOOLS)
	install -m 755 "$<" "$@"

$(LINUX_TOOLS)/management: | $(WORK)/management/.stamp $(LINUX_TOOLS)
	ln -sfn "$(abspath $(WORK)/management)" "$@"

LINUX_TOOL_NAMES := mke2fs e2fsck resize2fs debugfs
LINUX_TOOL_LINKS := $(addprefix $(LINUX_TOOLS)/,$(LINUX_TOOL_NAMES))
$(LINUX_TOOL_LINKS): $(LINUX_TOOLS)/run-tool | $(LINUX_TOOLS)/management
	ln -sfn run-tool "$@"

LINUX_TEST_PREREQUISITES := $(LINUX_TOOL_LINKS)
LINUX_TEST_ENV := CLANKERD_TEST_TOOLS=$(abspath $(LINUX_TOOLS))
endif

include build-support/checks.mk

clean:
	rm -rf build target
