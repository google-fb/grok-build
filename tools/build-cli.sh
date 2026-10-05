#!/usr/bin/env bash
# Build a committed revision without changing the source, seed, or host toolchain.
set -euo pipefail

die() { printf '%s\n' "$*" >&2; exit 2; }
if [[ $# -lt 4 || $# -gt 5 ]]; then
  die "usage: $0 SOURCE CACHE_SEED NEW_WORK_DIR NEW_OUTPUT_DIR [REVISION]"
fi
for tool in git docker realpath python3 sha256sum; do
  command -v "$tool" >/dev/null || die "missing command: $tool"
done
src=$(realpath -e -- "$1")
seed=$(realpath -e -- "$2")
work=$(realpath -m -- "$3")
out=$(realpath -m -- "$4")
revision=${5:-HEAD}
[[ -d "$src/.git" ]] || die 'SOURCE must be a standalone git clone'
[[ -d "$seed/cargo" ]] || die 'CACHE_SEED must contain cargo/'
[[ -z $(git -C "$src" status --porcelain=v1 --untracked-files=all) ]] || die 'SOURCE must be clean (including untracked files)'
commit=$(git -C "$src" rev-parse --verify --end-of-options "$revision^{commit}")
[[ ! -e "$work" && ! -L "$3" ]] || die 'NEW_WORK_DIR already exists'
[[ ! -e "$out" && ! -L "$4" ]] || die 'NEW_OUTPUT_DIR already exists'
for dest in "$work" "$out"; do
  for input in "$src" "$seed"; do
    [[ "$dest" != "$input" && "$dest" != "$input/"* ]] || die 'destination must be outside SOURCE and CACHE_SEED'
  done
done
[[ "$out" != "$work" && "$out" != "$work/"* && "$work" != "$out/"* ]] || die 'work and output must be disjoint'
# Docker --mount uses commas as delimiters, even inside a quoted argument.
[[ "$src$seed$work$out" != *','* && "$src$seed$work$out" != *$'\n'* ]] || die 'paths cannot contain commas or newlines'
[[ -d $(dirname -- "$work") && -d $(dirname -- "$out") ]] || die 'destination parents must exist'

image='rust@sha256:365468470075493dc4583f47387001854321c5a8583ea9604b297e67f01c5a4f'
docker image inspect "$image" --format '{{.Id}}' >/dev/null
mkdir -- "$work"
mkdir -- "$work/cache" "$work/artifacts"
git clone --quiet --no-hardlinks --no-checkout -- "$src" "$work/src"
git -C "$work/src" checkout --quiet --detach "$commit"
started=$(date -u +%FT%TZ)
docker image inspect "$image" --format '{{.Id}}' > "$work/artifacts/BUILDER-IMAGE.txt"
printf '%s\n' "$commit" > "$work/artifacts/SOURCE-COMMIT.txt"

# Only this new workspace is writable. The seed contributes downloads, never
# previously compiled objects. Keep it intact, including ownership and mtimes.
docker run --rm --pull=never --name "astra-cli-build-$$" --cpus 6 --memory 18g \
  --mount "type=bind,source=$work/src,target=/src,readonly" \
  --mount "type=bind,source=$seed,target=/seed,readonly" \
  --mount "type=bind,source=$work/cache,target=/build-cache" \
  --mount "type=bind,source=$work/artifacts,target=/artifacts" \
  -w /src -e CARGO_HOME=/build-cache/cargo -e CARGO_TARGET_DIR=/build-cache/build \
  -e PROTOC=/usr/bin/protoc -e CARGO_PROFILE_DEV_DEBUG=0 -e CARGO_INCREMENTAL=0 \
  -e GIT_CONFIG_COUNT=1 -e GIT_CONFIG_KEY_0=safe.directory -e GIT_CONFIG_VALUE_0=/src \
  -e "BUILD_OWNER=$(id -u):$(id -g)" "$image" bash -euo pipefail -c '
    trap '\''chown -R "$BUILD_OWNER" /build-cache /artifacts'\'' EXIT
    cp -a /seed/cargo /build-cache/cargo
    apt-get update
    apt-get install -y --no-install-recommends clang cmake ninja-build protobuf-compiler libclang-dev libssl-dev pkg-config nasm meson
    dpkg-query -W > /artifacts/BUILD-PACKAGES.txt
    rustc --version --verbose > /artifacts/RUSTC-VERSION.txt
    cargo --version > /artifacts/CARGO-VERSION.txt
    cc --version > /artifacts/CC-VERSION.txt
    ld --version > /artifacts/LD-VERSION.txt
    cargo build --locked -j 4 -p xai-grok-pager-bin
    cp /build-cache/build/debug/xai-grok-pager /artifacts/grok
    /artifacts/grok --version > /artifacts/VERSION.txt
    cd /artifacts
    sha256sum grok > SHA256SUMS
  ' > "$work/build.log" 2>&1

# Validate before publishing. --version ran in the container, without host auth
# or config mounts. A failed build leaves only its new work directory and log.
(cd "$work/artifacts" && sha256sum --check SHA256SUMS)
grep -Fq "(${commit:0:12})" "$work/artifacts/VERSION.txt" || die 'binary version does not match source commit'
python3 - "$work/artifacts" "$started" "$image" <<'PY'
import datetime, hashlib, json, pathlib, sys
artifacts = pathlib.Path(sys.argv[1])
paper = "29be1ea1a287324827cdb17908bde255ee6792b6851562404179728e90455b18"
with (artifacts / "grok").open("rb") as f:
    actual = hashlib.file_digest(f, "sha256").hexdigest()
manifest = {
    "source_commit": (artifacts / "SOURCE-COMMIT.txt").read_text().strip(),
    "builder_image": sys.argv[3], "started_utc": sys.argv[2],
    "finished_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "binary_sha256": actual, "paper_binary_sha256": paper,
    "matches_paper_binary": actual == paper,
    "version": (artifacts / "VERSION.txt").read_text().strip(),
    "profile": "dev", "dev_debug": 0, "incremental": False,
    "source_mount": "/src", "cargo_home": "/build-cache/cargo",
    "target_dir": "/build-cache/build", "compiled_cache_reused": False,
    "limitations": ["apt packages are recorded, not pinned",
                    "byte reproducibility is measured, not guaranteed"],
}
(artifacts / "BUILD-MANIFEST.json").write_text(json.dumps(manifest, indent=2) + "\n")
PY
# mkdir is the no-clobber boundary, including concurrent invocation.
mkdir -- "$out"
cp -a -- "$work/artifacts/." "$out/"
cat "$out/BUILD-MANIFEST.json"
printf 'Build log: %s/build.log\n' "$work"
