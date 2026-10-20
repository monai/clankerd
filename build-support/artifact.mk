# Install completed Cargo and package outputs using Make's file dependencies.
.DEFAULT_GOAL := artifact-install
.DELETE_ON_ERROR:
.PHONY: artifact-install artifact-publish check-output check-publication
ifndef STAGE
$(error STAGE must name an artifact output directory)
endif
include $(INPUT_LOCK)
include build-support/file-times.mk

HOST := $(STAGE)/darwin-arm64
GUEST := $(STAGE)/linux-arm64/management
LICENSES := $(STAGE)/licenses
PROGRAMS := vmctl clankerd-vmspawn clankerd-network
HOST_PROGRAMS := $(addprefix $(HOST)/,$(PROGRAMS))
GUEST_SOURCES := $(shell find $(WORK)/management -type f ! -name .stamp 2>/dev/null)
GUEST_FILES := $(patsubst $(WORK)/management/%,$(GUEST)/%,$(GUEST_SOURCES))
LICENSE_FILES := $(addprefix $(LICENSES)/texts/,$(addsuffix .txt,$(LICENSE_INPUTS)))
METADATA_PACKAGES := $(foreach input,$(INPUTS),$(if $(wildcard $(WORK)/packages/$(input)/package-metadata),$(input)))
PACKAGE_METADATA := $(addprefix $(LICENSES)/guest/,$(addsuffix /.stamp,$(METADATA_PACKAGES)))

ARTIFACT_FILES := $(HOST_PROGRAMS) $(GUEST_FILES) \
                 $(GUEST)/usr/sbin/clankerd-guestd $(STAGE)/inputs.mk \
                 $(LICENSE_FILES) $(PACKAGE_METADATA) \
                 $(LICENSES)/alpine-base-installed

artifact-install: $(ARTIFACT_FILES)
	python3 scripts/verify-host-artifact.py "$(STAGE)"

ifneq ($(PUBLISH),)
ifeq ($(shell uname -s),Darwin)
PUBLISH_MV := mv -fh
else
PUBLISH_MV := mv -fT
endif

check-publication:
	@test ! -e "$(PUBLISH)" || test -L "$(PUBLISH)" || { echo 'Publication path must be absent or a bundle symlink; remove the old output directory first: $(PUBLISH)' >&2; exit 1; }
	@if [ -L "$(PUBLISH)" ]; then \
	    case "$$(readlink "$(PUBLISH)")" in "$(notdir $(PUBLISH)).bundles/bundle."*) ;; \
	    *) echo 'Publication path points outside its bundle directory: $(PUBLISH)' >&2; exit 1;; esac; \
	fi

artifact-publish: $(PUBLISH)/.published
.PRECIOUS: $(PUBLISH)/.published

# Audit the working tree before copying it; never change a published bundle.
$(PUBLISH)/.published: $(ARTIFACT_FILES) $(STAGE)/.management-installed build-support/artifact.mk check-file-times | artifact-install check-publication
	@set -e; missing=; \
	for file in $(patsubst $(STAGE)/%,$(PUBLISH)/%,$(ARTIFACT_FILES)); do \
	    if [ ! -e "$$file" ] && [ ! -L "$$file" ]; then missing=1; break; fi; \
	done; \
	if [ -z "$$missing" ]; then $(skip_unchanged); fi; \
	mkdir -p "$(PUBLISH).bundles"; \
	bundle=$$(mktemp -d "$(PUBLISH).bundles/bundle.XXXXXX"); \
	link="$(PUBLISH).$${bundle##*/}.link"; target="$(notdir $(PUBLISH)).bundles/$${bundle##*/}"; \
	trap 'rm -f "$$link"; if [ "$$(readlink "$(PUBLISH)" 2>/dev/null || :)" != "$$target" ]; then rm -rf "$$bundle"; fi' 0; \
	trap 'exit 1' 1 2 15; \
	cp -pRP "$(STAGE)/." "$$bundle/"; \
	touch "$$bundle/.published"; \
	ln -s "$$target" "$$link"; \
	$(PUBLISH_MV) "$$link" "$(PUBLISH)"
endif

check-output:
	@test ! -L "$(STAGE)" || { echo 'Artifact output must be an ordinary directory' >&2; exit 1; }

$(STAGE) $(HOST) $(LICENSES) $(LICENSES)/texts $(LICENSES)/guest: | check-output
	mkdir -p "$@"

$(HOST_PROGRAMS): $(HOST)/%: $(CARGO_TARGET_DIR)/$(HOST_TARGET)/release/% build-support/artifact.mk | $(HOST)
	@set -e; $(skip_unchanged); \
	install -m 755 "$(CARGO_TARGET_DIR)/$(HOST_TARGET)/release/$(@F)" "$@.tmp"; \
	rcodesign sign --binary-identifier "io.clankerd.$(@F)" $(SIGN_FLAGS) "$@.tmp" || { rm -f "$@.tmp"; exit 1; }; \
	mv -f "$@.tmp" "$@"

$(HOST)/clankerd-vmspawn: SIGN_FLAGS := -e crates/clankerd-vmspawn/entitlements.plist
$(HOST)/clankerd-vmspawn: crates/clankerd-vmspawn/entitlements.plist

# A tree copy preserves directory permissions and empty package directories.
# Individual file targets below also let Make repair a missing installed file.
$(STAGE)/.management-installed: $(WORK)/management/.stamp build-support/artifact.mk | $(STAGE)
	@set -e; $(skip_unchanged); \
	rm -rf "$(GUEST)"; \
	mkdir -p "$(GUEST)"; \
	cp -pRP "$(WORK)/management/." "$(GUEST)/"; \
	rm "$(GUEST)/.stamp"; \
	touch "$@"

$(GUEST_FILES): $(GUEST)/%: $(WORK)/management/% | $(STAGE)/.management-installed
	@set -e; $(skip_unchanged); \
	cp -pP "$<" "$@.tmp"; \
	mv -f "$@.tmp" "$@"

$(GUEST)/usr/sbin/clankerd-guestd: $(CARGO_TARGET_DIR)/$(GUEST_TARGET)/release/clankerd-guestd $(STAGE)/.management-installed
	@set -e; $(skip_unchanged); \
	install -m 755 "$(CARGO_TARGET_DIR)/$(GUEST_TARGET)/release/clankerd-guestd" "$@"

$(STAGE)/inputs.mk: $(INPUT_LOCK) | $(STAGE)
	@set -e; $(skip_unchanged); install -m 644 "$<" "$@"

$(foreach input,$(LICENSE_INPUTS),$(eval $(LICENSES)/texts/$(input).txt: $(DOWNLOADS)/$(input)-$($(input)_SHA256).archive))
$(LICENSE_FILES): | $(LICENSES)/texts
	@set -e; $(skip_unchanged); install -m 644 "$<" "$@"

$(PACKAGE_METADATA): $(LICENSES)/guest/%/.stamp: $(WORK)/packages/%/.stamp | $(LICENSES)/guest
	@set -e; $(skip_unchanged); \
	rm -rf "$(@D)"; \
	cp -pRP "$(<D)/package-metadata" "$(@D)"; \
	touch "$@"

$(LICENSES)/alpine-base-installed: $(WORK)/management/lib/apk/db/installed | $(LICENSES)
	@set -e; $(skip_unchanged); install -m 644 "$<" "$@"

$(HOST_PROGRAMS) $(GUEST_FILES) \
$(GUEST)/usr/sbin/clankerd-guestd $(STAGE)/inputs.mk \
$(STAGE)/.management-installed \
$(LICENSE_FILES) $(PACKAGE_METADATA) $(LICENSES)/alpine-base-installed: check-file-times
