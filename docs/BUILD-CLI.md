# Isolated CLI rebuild

Run `bash tools/build-cli.sh SOURCE CACHE_SEED NEW_WORK_DIR NEW_OUTPUT_DIR [REVISION]`.
The destination parents must exist; both destinations must be new, disjoint
directories outside the source and cache seed. The source must be a clean,
standalone Git clone. A revision defaults to `HEAD`; uncommitted work is rejected.

For the published baseline, pass revision
`0bf4d4bd539a01099664ff97eccf361f5f75d65b`. The script clones that exact
revision into the new workspace. It does not switch or modify the source clone.

Requirements: Linux x86-64, Bash, Git, Docker, Python 3.11+, coreutils, the local
builder image `rust@sha256:365468470075493dc4583f47387001854321c5a8583ea9604b297e67f01c5a4f`,
and a seed directory containing Cargo's `cargo/` downloads. The build has a
6-CPU / 18-GiB container limit, uses four compiler jobs, and needs network access
for apt and any missing Cargo/build-script downloads. It never mounts host login
files or installs a host toolchain. Docker containers have an `astra-` prefix.

Only the Cargo download cache is copied from the read-only seed. Compiled objects
start empty. The source clone is mounted read-only at `/src`; compilation uses
`/build-cache/build`. The build uses `cargo build --locked -j 4 -p
xai-grok-pager-bin`, the dev profile with debug information and incremental
compilation disabled, and `/usr/bin/protoc`.

The output contains `grok`, `SHA256SUMS`, `VERSION.txt`, `SOURCE-COMMIT.txt`,
toolchain/package inventories, and `BUILD-MANIFEST.json`. The manifest compares
the new hash with the published binary hash without rewriting any existing pin.
`--version` executes inside the clean container. A host's release-channel marker
is not part of the artifact identity. No API call is made.

This is a repeatable build procedure, not a guarantee of identical bytes. Apt
versions are recorded but not pinned. Build time, dependency-generated data,
compiler/linker versions, and paths may affect bytes. A matching source version
does not establish binary equality. Preserve the manifest and inventories when
comparing builds. The script keeps its new workspace and build log after failure;
it publishes an output directory only after build and integrity checks succeed.

Safety regression tests (no Docker daemon or paid API needed):

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tools/tests -p 'test_build_cli.py' -v
```
