# Native build

Build host executables on Apple Silicon macOS with the tools in `mise.toml`,
Python 3.12+ and Apple's command-line SDK. Install libkrun, its runtime
dependencies (including libkrunfw), and liblzma through your system's normal
facilities before building. No package manager or installation prefix is assumed.
The native linker and dyld must be able to resolve them. The core build neither
checks library release versions nor discovers library search paths. Missing symbols,
libraries or incompatible ABIs fail through the native toolchain or loader.
Cargo's liblzma build-script overrides require installed libraries and suppress
source compilation on macOS ARM64 and Linux ARM64/x86_64 GNU hosts.

```sh
mise exec -- make build
```

For libraries installed through Homebrew, use the optional entrypoint:

```sh
mise exec -- make build-homebrew
```

It queries `brew --prefix` for libkrun, libkrunfw, xz and libkrun's installed
dependencies. It supplies native linker directories and embeds absolute runtime
search paths to these installed libraries in host binaries. No third-party
libraries are installed, copied, rewritten or signed. Guest compilation receives
no Homebrew flags. This delegates to the same `build` graph and publication.
The resulting host binaries still require those installed libraries, but should
run without `DYLD_*` variables. Moving them to another machine requires libraries
resolvable through the encoded paths or rebuilding for that installation.

The package-manager-independent `make build` continues to use the caller's
native toolchain configuration; it never invokes Homebrew. Host flag overrides
inside the adapter use Cargo's encoded argument format so library paths remain
single arguments. Cargo owns recompilation when those flags change.

Make retains checksum-verified guest downloads, guest management assembly,
Cargo compilation, executable signing and atomic publication. `build/artifact`
points to an immutable complete bundle in `build/artifact.bundles/`. Failed
builds leave the published bundle intact. A warm build reuses unchanged files.
The artifact includes clankerd's executables, Alpine guest management and guest
license metadata. Running it on another Mac requires installed host libraries
that its native loader can resolve.

Run consolidated native acceptance:

```sh
mise exec -- make test-acceptance
```

The matching Homebrew acceptance entrypoint builds with the same linker/runtime
configuration and removes inherited `DYLD_*` overrides before running tests:

```sh
mise exec -- make test-acceptance-homebrew
```

This tests runtime lookup from the built executables. Select particular gates
with `ACCEPTANCE_GATES=m2-smoke-lifetimes`; the common build, management and native
bind checks still run before the selected gate. `RUST_OUT` selects the same
publication path for builds and acceptance.

The script prints numbered progress, marks long steps and retains verbose logs
under `build/acceptance-<UTC>`. It checks script regressions, native compilation,
warm publication, executable signatures, real SDK compilation, management
isolation, bind semantics and VM gates. The VMM mapping check accepts installed
library paths and verifies the requesting user's process and loaded libkrun.
Library lookup failures remain in the logs and bounded failure summary.

```sh
mise exec -- python3 scripts/summarize-native-build.py
```

Linux developer checks require installed liblzma development/linker files and
e2fsprogs; guest compilation remains available with `make cargo-guest`.
Linux checks do not establish native Mac linkage, loader or VM behavior.
