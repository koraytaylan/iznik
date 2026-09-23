#!/usr/bin/env bash
# Install the Zig release this repository builds libghostty-vt with.
# The copy is official, checked against the SHA-256 ziglang.org publishes for
# it, and unpacked onto PATH. It is not stored in the Actions cache: that
# cache client answers 400 and the download is small.
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

# The digests https://ziglang.org/download/index.json lists for this version.
# A case rather than an associative array: macOS runs bash 3.2. Changing the
# version means replacing every line here from that index.
case "${arch}-${platform}" in
  x86_64-linux) digest="02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239" ;;
  aarch64-linux) digest="958ed7d1e00d0ea76590d27666efbf7a932281b3d7ba0c6b01b0ff26498f667f" ;;
  x86_64-macos) digest="375b6909fc1495d16fc2c7db9538f707456bfc3373b14ee83fdd3e22b3d43f7f" ;;
  aarch64-macos) digest="3cc2bab367e185cdfb27501c4b30b1b0653c28d9f73df8dc91488e66ece5fa6b" ;;
  x86_64-windows) digest="3a0ed1e8799a2f8ce2a6e6290a9ff22e6906f8227865911fb7ddedc3cc14cb0c" ;;
  *)
    echo "install-zig: no published digest recorded for ${arch}-${platform}" >&2
    exit 1
    ;;
esac

name="zig-${arch}-${platform}-${version}"
archive="${name}.tar.xz"
if [[ "$platform" == "windows" ]]; then
  archive="${name}.zip"
fi
url="https://ziglang.org/download/${version}/${archive}"
# Git Bash leaves RUNNER_TEMP as D:\... . GNU tar reads a colon in the
# archive name as a remote host, so the work directory is a POSIX path
# before anything is downloaded into it.
root="${RUNNER_TEMP:?}/zig"
if command -v cygpath >/dev/null 2>&1; then
  root="$(cygpath -u -- "$root")"
fi
mkdir -p "$root"
archive_path="${root}/${archive}"
curl --fail --silent --show-error --location --output "$archive_path" "$url"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$archive_path")"
else
  actual="$(shasum -a 256 "$archive_path")"
fi
actual="${actual%% *}"
if [[ "$actual" != "$digest" ]]; then
  echo "install-zig: ${archive} has SHA-256 ${actual}, expected ${digest}" >&2
  exit 1
fi
if [[ "$platform" == "windows" ]]; then
  # The tar on PATH is GNU tar, and it cannot read the official zip.
  # Windows ships bsdtar, which extracts that zip and keeps a drive
  # letter as a local path. MSYS must not rewrite those arguments.
  windows_archive="$(cygpath -w -- "$archive_path")"
  windows_root="$(cygpath -w -- "$root")"
  windows_tar="$(cygpath -w -- /c/Windows/System32/tar.exe)"
  MSYS_NO_PATHCONV=1 "$windows_tar" -C "$windows_root" -xf "$windows_archive"
else
  tar --extract --file "$archive_path" --directory "$root"
fi
installed="${root}/${name}"
binary="${installed}/zig"
if [[ "$platform" == "windows" ]]; then
  binary="${binary}.exe"
  installed="$(cygpath -w -- "$installed")"
fi
echo "$installed" >> "${GITHUB_PATH:?}"
"$binary" version
