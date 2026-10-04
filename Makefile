# Pure Go (no cgo); build everything from inside the dev container.
# Go is pinned in ../mise.toml: `mise install` once, then `mise exec -- make`.
export CGO_ENABLED := 0
VERSION ?= $(shell git describe --tags --always --dirty 2>/dev/null || echo dev)
LDFLAGS := -s -w -X github.com/monai/clankers/sandbox/clankerd/internal/cli.Version=$(VERSION)
TARGETS := darwin-arm64 linux-arm64

.PHONY: build test vet clean
build:
	@for t in $(TARGETS); do \
	  for p in clankerd clankerctl; do \
	    GOOS=$${t%-*} GOARCH=$${t#*-} go build -trimpath -ldflags "$(LDFLAGS)" -o build/$$t/$$p ./cmd/$$p || exit 1; \
	  done; \
	done

vet:
	go vet ./...

test:
	go test -count=1 ./...

clean:
	rm -rf build
