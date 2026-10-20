.DEFAULT_GOAL := build
.PHONY: build clean

# mise install supplies the pinned toolchain; all build phases run in one context.
# Guest distribution inputs and assembly phases are real graph nodes.
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
include build-support/file-times.mk
PYTHON := python3
.DELETE_ON_ERROR:
HOST_TARGET := aarch64-apple-darwin
GUEST_TARGET := aarch64-unknown-linux-musl
HOST_EXECUTABLES := $(addprefix $(CARGO_TARGET_DIR)/$(HOST_TARGET)/release/,vmctl clankerd-vmspawn clankerd-network)
GUEST_EXECUTABLE := $(CARGO_TARGET_DIR)/$(GUEST_TARGET)/release/clankerd-guestd

.PHONY: verify-inputs

# A failing downloader must not let Make delete a peer's verified archive.
define binary_input
.PRECIOUS: $(DOWNLOADS)/$(1)-$($(1)_SHA256).archive
$(DOWNLOADS)/$(1)-$($(1)_SHA256).archive:
	@set -e; mkdir -p "$$(@D)"; \
	temporary=$$$$(mktemp "$$@.tmp.XXXXXX"); \
	trap 'rm -f "$$$$temporary"' 0; \
	trap 'exit 1' 1 2 15; \
	curl --fail --location --retry 3 --output "$$$$temporary" '$($(1)_URL)'; \
	printf '%s  %s\n' '$($(1)_SHA256)' "$$$$temporary" | shasum -a 256 -c -; \
	mv -f "$$$$temporary" "$$@"
$(WORK)/packages/$(1)/.stamp: $(DOWNLOADS)/$(1)-$($(1)_SHA256).archive scripts/extract-binary.py Makefile check-file-times
	@set -e; $$(skip_unchanged); \
	printf '%s  %s\n' '$($(1)_SHA256)' "$$<" | shasum -a 256 -c -; \
	rm -rf "$$(@D)"; \
	$(PYTHON) scripts/extract-binary.py "$$<" "$$(@D)"; \
	touch "$$@"
endef
$(foreach input,$(INPUTS),$(eval $(call binary_input,$(input))))
verify-inputs: $(foreach input,$(INPUTS),$(DOWNLOADS)/$(input)-$($(input)_SHA256).archive)
	$(foreach input,$(INPUTS),printf '%s  %s\n' '$($(input)_SHA256)' '$(DOWNLOADS)/$(input)-$($(input)_SHA256).archive' | shasum -a 256 -c - && ) true
GUEST_PACKAGES := $(addprefix $(WORK)/packages/,$(GUEST_INPUTS))
MANAGEMENT_ROOTFS := build-support/management-rootfs
MANAGEMENT_FILES := $(shell find $(MANAGEMENT_ROOTFS))

$(WORK)/management/.stamp: $(addsuffix /.stamp,$(GUEST_PACKAGES)) $(MANAGEMENT_FILES) $(INPUT_LOCK) scripts/assemble-management.py Makefile check-file-times
	@set -e; $(skip_unchanged); \
	rm -rf "$(@D)"; \
	$(PYTHON) scripts/assemble-management.py "$(@D)" $(MANAGEMENT_ROOTFS) $(GUEST_PACKAGES) --required $(GUEST_REQUIRED); \
	touch "$@"

.PHONY: cargo-host cargo-guest
cargo-host:
	@test "$(shell uname -sm)" = 'Darwin arm64' || { echo 'Host executables require native Apple Silicon macOS' >&2; exit 1; }
	+cargo build --locked --release --target $(HOST_TARGET) -p vmctl -p clankerd-vmspawn -p clankerd-network

cargo-guest:
	+cargo build --locked --release --target $(GUEST_TARGET) --config 'target.$(GUEST_TARGET).linker="rust-lld"' -p clankerd-guestd

$(HOST_EXECUTABLES): cargo-host
	@test -f "$@"

$(GUEST_EXECUTABLE): cargo-guest
	@test -f "$@"

LICENSE_ARCHIVES := $(foreach input,$(LICENSE_INPUTS),$(DOWNLOADS)/$(input)-$($(input)_SHA256).archive)

build: $(LICENSE_ARCHIVES) $(HOST_EXECUTABLES) $(GUEST_EXECUTABLE) $(WORK)/management/.stamp
	+$(MAKE) --no-print-directory -f build-support/artifact.mk artifact-publish STAGE="$(WORK)/artifact" PUBLISH="$(RUST_OUT)" WORK="$(WORK)" INPUT_LOCK="$(INPUT_LOCK)" DOWNLOADS="$(DOWNLOADS)" HOST_TARGET="$(HOST_TARGET)" GUEST_TARGET="$(GUEST_TARGET)"

ifeq ($(BUILD_CONTEXT),Linux-aarch64)
LINUX_TOOLS := $(WORK)/linux-test-tools
$(LINUX_TOOLS):
	mkdir -p "$@"

$(LINUX_TOOLS)/run-tool: build-support/linux-test-tools/run-tool check-file-times | $(LINUX_TOOLS)
	@set -e; $(skip_unchanged); install -m 755 "$<" "$@"

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
