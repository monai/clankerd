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
mise exec -- cmake --preset default
mise exec -- cmake --build --preset default --target build
```

For libraries installed through Homebrew, use the optional entrypoint:

```sh
mise exec -- cmake --preset homebrew
mise exec -- cmake --build --preset homebrew --target build
```

It queries `brew --prefix` for libkrun, libkrunfw, xz and libkrun's installed
dependencies. It supplies native linker directories and embeds absolute runtime
search paths to these installed libraries in host binaries. No third-party
libraries are installed, copied, rewritten or signed. Guest compilation receives
no Homebrew flags. This delegates to the same `build` graph and publication.
The resulting host binaries still require those installed libraries, but should
run without `DYLD_*` variables. Moving them to another machine requires libraries
resolvable through the encoded paths or rebuilding for that installation.

The package-manager-independent `cmake --build --preset default --target build` continues to use the caller's
native toolchain configuration; it never invokes Homebrew. Host flag overrides
in CMake use Cargo's encoded argument format so library paths remain
single arguments. Cargo owns recompilation when those flags change.

CMake owns checksum-verified guest downloads, assembly dependencies, executable
signing and atomic publication. Corrosion invokes Cargo for Rust compilation.
Cargo also keeps ownership of crate-local C compilation and native linking.
The two retained Python build scripts implement archive safety and guest
filesystem policy. They preserve package metadata, symlinks, overlay order,
permissions and architecture checks. These operations remain in Python because
they require product-specific traversal and validation. The small shell publication
script keeps signal cleanup and atomic symlink replacement together. `build/artifact`
points to an immutable complete bundle in `build/artifact.bundles/`. Failed
builds leave the published bundle intact. A warm build reuses unchanged files.
The artifact includes clankerd's executables, Alpine guest management and guest
license metadata. Running it on another Mac requires installed host libraries
that its native loader can resolve.

Run consolidated native acceptance:

```sh
mise exec -- bash scripts/accept-native-build.sh
```

The script configures the Homebrew preset, runs lint and ordinary tests, builds
and signs the bundle, checks a warm rebuild, and runs real VM acceptance.
Set `CLANKERD_CMAKE_PRESET=default` to use caller-configured native libraries.
Pass gate names as script arguments to select a subset. Common management and
native bind checks still run. `CLANKERD_ARTIFACT_DIR` selects the publication path.

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
e2fsprogs; guest compilation remains available with `cmake --build --preset default --target cargo-guest`.
Linux checks do not establish native Mac linkage, loader or VM behavior.
