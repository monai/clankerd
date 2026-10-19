# Install completed Cargo and package outputs using Make's file dependencies.
.DEFAULT_GOAL := artifact-install
.DELETE_ON_ERROR:
.PHONY: artifact-install host-libraries-install check-output
ifndef STAGE
$(error STAGE must name an artifact output directory)
endif
include $(INPUT_LOCK)

HOST := $(STAGE)/darwin-arm64
GUEST := $(STAGE)/linux-arm64/management
LICENSES := $(STAGE)/licenses
PROGRAMS := vmctl clankerd-vmspawn clankerd-network
HOST_PROGRAMS := $(addprefix $(HOST)/,$(PROGRAMS))
HOST_LIBRARIES := $(addprefix $(HOST)/lib/,$(notdir $(wildcard $(WORK)/host-libraries/lib/*.dylib)))
GUEST_SOURCES := $(shell find $(WORK)/management -type f ! -name .stamp 2>/dev/null)
GUEST_FILES := $(patsubst $(WORK)/management/%,$(GUEST)/%,$(GUEST_SOURCES))
LICENSE_FILES := $(addprefix $(LICENSES)/texts/,$(addsuffix .txt,$(LICENSE_INPUTS)))
METADATA_PACKAGES := $(foreach input,$(INPUTS),$(if $(wildcard $(WORK)/packages/$(input)/package-metadata),$(input)))
PACKAGE_METADATA := $(addprefix $(LICENSES)/guest/,$(addsuffix /.stamp,$(METADATA_PACKAGES)))

# Make 3.81 can miss updates within one second. find compares nanoseconds;
# check every Cargo output, but leave an unchanged installed executable alone.
.PHONY: check-executable-times
check-executable-times:

define skip_unchanged_executable
if [ -f "$@" ]; then \
    newer=$$(find $(foreach prerequisite,$(filter-out check-executable-times,$^),"$(prerequisite)") -prune -newer "$@" -print) || exit 1; \
    [ -n "$$newer" ] || exit 0; \
fi
endef

$(HOST_PROGRAMS) $(GUEST)/usr/sbin/clankerd-guestd: check-executable-times

host-libraries-install: $(HOST_LIBRARIES)

artifact-install: $(HOST_PROGRAMS) $(HOST_LIBRARIES) $(GUEST_FILES) \
                 $(GUEST)/usr/sbin/clankerd-guestd $(STAGE)/inputs.mk \
                 $(STAGE)/.host-licenses $(LICENSE_FILES) $(PACKAGE_METADATA) \
                 $(LICENSES)/alpine-base-installed
	python3 scripts/verify-host-artifact.py "$(STAGE)"

check-output:
	@test ! -L "$(STAGE)" || { echo 'Artifact output must be an ordinary directory' >&2; exit 1; }

$(STAGE) $(HOST) $(HOST)/lib $(LICENSES) $(LICENSES)/texts $(LICENSES)/guest: | check-output
	mkdir -p "$@"

$(HOST_PROGRAMS): $(HOST)/%: $(CARGO_TARGET_DIR)/$(HOST_TARGET)/release/% build-support/artifact.mk | $(HOST)
	@set -e; $(skip_unchanged_executable); \
	install -m 755 "$(CARGO_TARGET_DIR)/$(HOST_TARGET)/release/$(@F)" "$@.tmp"; \
	rcodesign sign --binary-identifier "io.clankerd.$(@F)" $(SIGN_FLAGS) "$@.tmp" || { rm -f "$@.tmp"; exit 1; }; \
	mv -f "$@.tmp" "$@"

$(HOST)/clankerd-vmspawn: SIGN_FLAGS := -e crates/clankerd-vmspawn/entitlements.plist
$(HOST)/clankerd-vmspawn: crates/clankerd-vmspawn/entitlements.plist

$(HOST_LIBRARIES): $(HOST)/lib/%: $(WORK)/host-libraries/lib/% build-support/artifact.mk | $(HOST)/lib
	cp -pP "$<" "$@.tmp"
	if [ ! -L "$@.tmp" ]; then rcodesign sign "$@.tmp" || { rm -f "$@.tmp"; exit 1; }; fi
	mv -f "$@.tmp" "$@"

# A tree copy preserves directory permissions and empty package directories.
# Individual file targets below also let Make repair a missing installed file.
$(STAGE)/.management-installed: $(WORK)/management/.stamp build-support/artifact.mk | $(STAGE)
	rm -rf "$(GUEST)"
	mkdir -p "$(GUEST)"
	cp -pRP "$(WORK)/management/." "$(GUEST)/"
	rm "$(GUEST)/.stamp"
	touch "$@"

$(GUEST_FILES): $(GUEST)/%: $(WORK)/management/% | $(STAGE)/.management-installed
	cp -pP "$<" "$@.tmp"
	mv -f "$@.tmp" "$@"

$(GUEST)/usr/sbin/clankerd-guestd: $(CARGO_TARGET_DIR)/$(GUEST_TARGET)/release/clankerd-guestd $(STAGE)/.management-installed
	@set -e; $(skip_unchanged_executable); \
	install -m 755 "$(CARGO_TARGET_DIR)/$(GUEST_TARGET)/release/clankerd-guestd" "$@"

$(STAGE)/inputs.mk: $(INPUT_LOCK) | $(STAGE)
	install -m 644 "$<" "$@"

$(STAGE)/.host-licenses: $(WORK)/host-libraries/.stamp | $(LICENSES)
	rm -rf "$(LICENSES)/host"
	cp -pRP "$(WORK)/host-libraries/licenses" "$(LICENSES)/host"
	touch "$@"

$(foreach input,$(LICENSE_INPUTS),$(eval $(LICENSES)/texts/$(input).txt: $(DOWNLOADS)/$(input)-$($(input)_SHA256).archive))
$(LICENSE_FILES): | $(LICENSES)/texts
	install -m 644 "$<" "$@"

$(PACKAGE_METADATA): $(LICENSES)/guest/%/.stamp: $(WORK)/packages/%/.stamp | $(LICENSES)/guest
	rm -rf "$(@D)"
	cp -pRP "$(<D)/package-metadata" "$(@D)"
	touch "$@"

$(LICENSES)/alpine-base-installed: $(WORK)/management/lib/apk/db/installed | $(LICENSES)
	install -m 644 "$<" "$@"
