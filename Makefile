export CGO_ENABLED := 0
VERSION ?= $(shell git describe --tags --always --dirty 2>/dev/null || echo dev)
LDFLAGS := -s -w -X github.com/monai/clankers/clankerd/internal/cli.Version=$(VERSION)
TARGETS := darwin-arm64 linux-arm64

.PHONY: build test vet clean rust-build rust-sign rust-verify rust-test rust-lint rust
build:
	@for t in $(TARGETS); do \
	  progs="clankerd hostctl"; \
	  [ $$t = linux-arm64 ] && progs="$$progs guestctl"; \
	  for p in $$progs; do \
	    GOOS=$${t%-*} GOARCH=$${t#*-} go build -trimpath -ldflags "$(LDFLAGS)" -o build/$$t/$$p ./cmd/$$p || exit 1; \
	  done; \
	done

vet:
	go vet ./...

test:
	go test -count=1 ./...

# Rust workspace. Prerequisite: `mise install`. One command: `make rust`
# (build + sign + verify); outputs land in build/rust/<os-arch>/.
RUST_OUT := build/rust
DARWIN := aarch64-apple-darwin
MUSL := aarch64-unknown-linux-musl

rust-lint:
	cargo fmt --check
	cargo clippy --workspace --all-targets -- -D warnings

rust-test:
	cargo test --workspace

# vmctl links Security.framework and CoreFoundation (TLS certificate
# verification via rustls-platform-verifier). Linux has no macOS SDK, so the
# darwin link uses an SDK root holding link-time stubs of just the symbols we
# import (scripts/macos-sdk-stubs); dyld binds them to the real frameworks.
rust-build:
	SDKROOT=$(CURDIR)/scripts/macos-sdk-stubs cargo zigbuild --release --target $(DARWIN) -p vmctl -p clankerd-vmspawn
	cargo zigbuild --release --target $(MUSL) -p clankerd-guestd
	mkdir -p $(RUST_OUT)/darwin-arm64 $(RUST_OUT)/linux-arm64
	cp target/$(DARWIN)/release/vmctl target/$(DARWIN)/release/clankerd-vmspawn $(RUST_OUT)/darwin-arm64/
	cp target/$(MUSL)/release/clankerd-guestd $(RUST_OUT)/linux-arm64/
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
