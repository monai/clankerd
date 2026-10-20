# Native build

Build host executables on Apple Silicon macOS with the tools in `mise.toml`,
Python 3.12+ and Apple's command-line SDK. Install libkrun, its runtime
dependencies (including libkrunfw), and liblzma through your system's normal
facilities before building. No package manager or installation prefix is assumed.
The native linker and dyld must be able to resolve them. The application neither
checks library release versions nor adds search paths. Missing symbols,
libraries or incompatible ABIs fail through the native toolchain or loader.
Cargo's liblzma build-script overrides require installed libraries and suppress
source compilation on macOS ARM64 and Linux ARM64/x86_64 GNU hosts.

```sh
mise exec -- make build
```

Make retains checksum-verified guest downloads, guest management assembly,
Cargo compilation, executable signing and atomic publication. `build/artifact`
points to an immutable complete bundle in `build/artifact.bundles/`. Failed
builds leave the published bundle intact. A warm build reuses unchanged files.
The artifact includes clankerd's executables, Alpine guest management and guest
license metadata. Running it on another Mac requires installed host libraries
that its native loader can resolve.

Run consolidated native acceptance:

```sh
mise exec -- bash scripts/accept-native-build.sh
```

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
