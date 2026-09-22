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
