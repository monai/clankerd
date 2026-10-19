# Install one private staging tree. The main graph supplies completed inputs;
# publication happens only after this Make invocation succeeds.
.DEFAULT_GOAL := artifact-install
.PHONY: artifact-install
ifndef STAGE
$(error STAGE must name a private artifact staging directory)
endif
include $(INPUT_LOCK)

HOST := $(STAGE)/darwin-arm64
GUEST := $(STAGE)/linux-arm64/management
LICENSES := $(STAGE)/licenses
PROGRAMS := vmctl clankerd-vmspawn clankerd-network
HOST_PROGRAMS := $(addprefix $(HOST)/,$(PROGRAMS))
HOST_LIBRARIES := $(addprefix $(HOST)/lib/,$(notdir $(wildcard $(WORK)/host-libraries/lib/*.dylib)))
LICENSE_FILES := $(addprefix $(LICENSES)/texts/,$(addsuffix .txt,$(LICENSE_INPUTS)))
METADATA_PACKAGES := $(foreach input,$(INPUTS),$(if $(wildcard $(WORK)/packages/$(input)/package-metadata),$(input)))
PACKAGE_METADATA := $(addprefix $(LICENSES)/guest/,$(METADATA_PACKAGES))

artifact-install: $(HOST_PROGRAMS) $(HOST_LIBRARIES) \
                 $(GUEST)/usr/sbin/clankerd-guestd $(STAGE)/inputs.mk \
                 $(LICENSES)/host $(LICENSE_FILES) $(PACKAGE_METADATA) \
                 $(LICENSES)/alpine-base-installed
	python3 scripts/verify-host-artifact.py "$(STAGE)"

$(STAGE) $(HOST) $(HOST)/lib $(LICENSES) $(LICENSES)/texts $(LICENSES)/guest:
	mkdir -p "$@"

$(HOST_PROGRAMS): $(HOST)/%: $(WORK)/host-rust/% | $(HOST)
	install -m 755 "$(WORK)/host-rust/$(@F)" "$@"
	rcodesign sign --binary-identifier "io.clankerd.$(@F)" $(SIGN_FLAGS) "$@"

$(HOST)/clankerd-vmspawn: SIGN_FLAGS := -e crates/clankerd-vmspawn/entitlements.plist
$(HOST)/clankerd-vmspawn: crates/clankerd-vmspawn/entitlements.plist

$(HOST_LIBRARIES): $(HOST)/lib/%: $(WORK)/host-libraries/lib/% | $(HOST)/lib
	cp -pP "$<" "$@"
	if [ ! -L "$@" ]; then rcodesign sign "$@"; fi

$(GUEST): $(WORK)/management/.complete
	mkdir -p "$@"
	cp -pRP "$(WORK)/management/." "$@/"
	rm "$@/.complete"

$(GUEST)/usr/sbin/clankerd-guestd: $(WORK)/guest-rust/clankerd-guestd | $(GUEST)
	install -m 755 "$<" "$@"

$(STAGE)/inputs.mk: $(INPUT_LOCK) | $(STAGE)
	install -m 644 "$<" "$@"

$(LICENSES)/host: $(WORK)/host-libraries/.complete | $(LICENSES)
	cp -pRP "$(WORK)/host-libraries/licenses" "$@"

$(foreach input,$(LICENSE_INPUTS),$(eval $(LICENSES)/texts/$(input).txt: $(DOWNLOADS)/$(input)-$($(input)_SHA256).archive))
$(LICENSE_FILES): | $(LICENSES)/texts
	install -m 644 "$<" "$@"

$(PACKAGE_METADATA): $(LICENSES)/guest/%: $(WORK)/packages/%/.complete | $(LICENSES)/guest
	cp -pRP "$(<D)/package-metadata" "$@"

$(LICENSES)/alpine-base-installed: $(WORK)/management/lib/apk/db/installed | $(LICENSES)
	install -m 644 "$<" "$@"
