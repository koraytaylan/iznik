#!/usr/bin/env bash
# Install the Zig release this repository builds libghostty-vt with.
# The copy is official and unpacked onto PATH. It is not stored in the
# Actions cache: that cache client answers 400 and the download is small.
set -euo pipefail

version="0.15.2"
system="$(uname -s)"
machine="$(uname -m)"

case "$system" in
  Linux)
    platform="linux"
    arch="$machine"
    ;;
  Darwin)
    platform="macos"
    case "$machine" in
      arm64) arch="aarch64" ;;
      *) arch="$machine" ;;
    esac
    ;;
  MINGW* | MSYS* | CYGWIN*)
    platform="windows"
    arch="x86_64"
    ;;
  *)
    echo "install-zig: unsupported system ${system}" >&2
    exit 1
    ;;
esac

name="zig-${arch}-${platform}-${version}"
archive="${name}.tar.xz"
if [[ "$platform" == "windows" ]]; then
  archive="${name}.zip"
fi
url="https://ziglang.org/download/${version}/${archive}"
root="${RUNNER_TEMP}/zig"
mkdir -p "$root"
curl --fail --silent --show-error --location --output "${root}/${archive}" "$url"
tar --extract --file "${root}/${archive}" --directory "$root"
echo "${root}/${name}" >> "$GITHUB_PATH"
"${root}/${name}/zig" version
