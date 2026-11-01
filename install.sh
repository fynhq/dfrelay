#!/bin/sh
# DFRelay installer: installs dfr from a local binary or a GitHub release.
set -eu

REPO="fynhq/dfrelay"
BIN_NAME="dfr"
SYSTEM_DIR="/usr/local/bin"

usage() {
  cat <<'EOF'
Usage: install.sh [options] [path-to-dfr]

Install the DFRelay executable.

Options:
  -h, --help           Show this help.

When path-to-dfr is omitted, the dfr file next to this script is used if it
exists. Otherwise the latest GitHub release for this platform is downloaded
and verified against the release SHA256SUMS file.

dfr is installed to /usr/local/bin when that directory can be created and
written; otherwise ~/.local/bin is used. On Termux, the Android arm64 build is
installed to $PREFIX/bin. Missing directories are created. Re-running the
installer replaces the existing dfr executable (upgrade). The installer never
calls sudo.
EOF
}

die() {
  printf 'install.sh: %s\n' "$*" >&2
  exit 1
}

say() {
  printf '%s\n' "$*"
}

warn() {
  printf 'install.sh: warning: %s\n' "$*" >&2
}

tmpdir=""
cleanup() {
  if [ -n "$tmpdir" ] && [ -d "$tmpdir" ]; then
    rm -rf -- "$tmpdir"
  fi
}
trap cleanup 0
trap 'exit 1' HUP INT TERM

[ -n "${HOME:-}" ] || die "HOME is not set"

is_termux=0
if [ -n "${TERMUX_VERSION:-}" ]; then
  is_termux=1
fi
case "${PREFIX:-}" in
  *com.termux*) is_termux=1 ;;
esac
if [ "$is_termux" -eq 1 ]; then
  SYSTEM_DIR="${PREFIX:-$HOME/.local}/bin"
fi

BIN_PATH=""
VERSION=""
while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      [ $# -eq 1 ] || die "-- requires exactly one binary path"
      BIN_PATH=$1
      shift
      ;;
    -*)
      die "unknown option: $1"
      ;;
    *)
      [ -z "$BIN_PATH" ] || die "unexpected extra argument: $1"
      BIN_PATH=$1
      shift
      ;;
  esac
done

script_dir=""
if [ -f "$0" ]; then
  case "$0" in
    */*) script_path=$0 ;;
    *) script_path="./$0" ;;
  esac
  script_dir=$(CDPATH= cd -P "$(dirname "$script_path")" 2>/dev/null && pwd -P) || script_dir=""
fi

if [ -z "$BIN_PATH" ] && [ -n "$script_dir" ] && [ -f "$script_dir/$BIN_NAME" ]; then
  BIN_PATH="$script_dir/$BIN_NAME"
fi

if [ -n "$BIN_PATH" ]; then
  [ -f "$BIN_PATH" ] || die "binary not found: $BIN_PATH"
  [ -r "$BIN_PATH" ] || die "binary is not readable: $BIN_PATH"
fi

EXISTING_PATH=""
EXISTING_PATHS=""

is_dfrelay_binary() {
  candidate=$1
  [ -f "$candidate" ] || return 1
  [ -x "$candidate" ] || return 1
  candidate_help=$("$candidate" --help 2>/dev/null || true)
  case "$candidate_help" in
    *DFRelay*) return 0 ;;
  esac
  return 1
}

add_existing_path() {
  candidate=$1
  case "$candidate" in
    /*) ;;
    *) return 0 ;;
  esac
  is_dfrelay_binary "$candidate" || return 0
  case "|$EXISTING_PATHS|" in
    *"|$candidate|"*) return 0 ;;
  esac
  if [ -n "$EXISTING_PATHS" ]; then
    EXISTING_PATHS="$EXISTING_PATHS|$candidate"
  else
    EXISTING_PATHS=$candidate
  fi
}

list_has_path() {
  case "|$EXISTING_PATHS|" in
    *"|$1|"*) return 0 ;;
  esac
  return 1
}

detect_existing_install() {
  effective_path=$(command -v "$BIN_NAME" 2>/dev/null || true)
  case "$effective_path" in
    /*) add_existing_path "$effective_path" ;;
  esac
  add_existing_path "$SYSTEM_DIR/$BIN_NAME"
  add_existing_path "$HOME/.local/bin/$BIN_NAME"

  if [ -n "$effective_path" ] && list_has_path "$effective_path"; then
    EXISTING_PATH=$effective_path
  elif list_has_path "$SYSTEM_DIR/$BIN_NAME"; then
    EXISTING_PATH="$SYSTEM_DIR/$BIN_NAME"
  elif list_has_path "$HOME/.local/bin/$BIN_NAME"; then
    EXISTING_PATH="$HOME/.local/bin/$BIN_NAME"
  fi
}

detect_existing_install

if [ -n "$EXISTING_PATH" ]; then
  existing_dir=$(dirname "$EXISTING_PATH")
  if [ ! -w "$existing_dir" ] || [ ! -x "$existing_dir" ]; then
    say "Existing DFRelay installation: $EXISTING_PATH"
    if [ "$(id -u 2>/dev/null || printf 1)" -eq 0 ]; then
      die "cannot replace it because $existing_dir is not writable"
    fi
    die "cannot replace it because $existing_dir is not writable; re-run this script with sudo to upgrade it"
  fi
fi

fetch() {
  fetch_url=$1
  fetch_dest=$2
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$fetch_url" -o "$fetch_dest"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$fetch_dest" "$fetch_url"
  else
    die "curl or wget is required to download DFRelay"
  fi
}

verify_checksum() {
  checksum_archive=$1
  checksum_file=$2
  checksum_name=$3
  expected=$(awk -v name="$checksum_name" '($2 == name || $2 == "*" name) { print tolower($1); exit }' "$checksum_file")
  [ -n "$expected" ] || die "SHA256SUMS does not list $checksum_name"
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$checksum_archive" | awk '{ print tolower($1); exit }')
  elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$checksum_archive" | awk '{ print tolower($1); exit }')
  else
    die "sha256sum or shasum is required to verify the download"
  fi
  [ "$actual" = "$expected" ] || die "checksum mismatch for $checksum_name"
}

resolve_latest_version() {
  latest_json="$tmpdir/latest.json"
  fetch "https://api.github.com/repos/$REPO/releases/latest" "$latest_json" ||
    die "failed to query the latest release"
  VERSION=$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"v\{0,1\}\([^"]*\)".*/\1/p' "$latest_json" | head -n 1)
  [ -n "$VERSION" ] || die "could not determine the latest version"
}

detect_platform() {
  detected_os=$(uname -s)
  detected_arch=$(uname -m)
  if [ "$is_termux" -eq 1 ]; then
    os=android
  else
    case "$detected_os" in
      Linux) os=linux ;;
      Darwin) os=macos ;;
      *) die "unsupported operating system: $detected_os" ;;
    esac
  fi
  case "$detected_arch" in
    x86_64|amd64) arch=x64 ;;
    aarch64|arm64) arch=arm64 ;;
    *) die "unsupported architecture: $detected_arch" ;;
  esac
  if [ "$os" = android ] && [ "$arch" != arm64 ]; then
    die "Termux on $detected_arch is not supported; only arm64 is published"
  fi
}

build_asset_name() {
  case "$os" in
    linux) ASSET="dfrelay_linux_${arch}_v${VERSION}_musl.tar.gz" ;;
    android) ASSET="dfrelay_android_${arch}_v${VERSION}.tar.gz" ;;
    *) ASSET="dfrelay_macos_${arch}_v${VERSION}.tar.gz" ;;
  esac
}

download_release() {
  release_base="https://github.com/$REPO/releases/download/v${VERSION}"
  archive="$tmpdir/$ASSET"
  sums="$tmpdir/SHA256SUMS"
  fetch "$release_base/$ASSET" "$archive" || die "failed to download $ASSET"
  fetch "$release_base/SHA256SUMS" "$sums" || die "failed to download SHA256SUMS"
  verify_checksum "$archive" "$sums" "$ASSET"
  mkdir -p "$tmpdir/unpack" || die "cannot create extraction directory"
  tar -xzf "$archive" -C "$tmpdir/unpack" || die "failed to extract $ASSET"
  [ -f "$tmpdir/unpack/$BIN_NAME" ] || die "$ASSET does not contain $BIN_NAME"
  DOWNLOADED_BIN="$tmpdir/unpack/$BIN_NAME"
}

if [ -z "$BIN_PATH" ]; then
  command -v tar >/dev/null 2>&1 || die "tar is required to extract the release archive"
  detect_platform
  tmpdir=$(mktemp -d "${TMPDIR:-/tmp}/dfrelay.XXXXXX") || die "cannot create temporary directory"
  resolve_latest_version
  build_asset_name
  say "Downloading DFRelay v$VERSION for $os-$arch..."
  download_release
  BIN_PATH=$DOWNLOADED_BIN
fi

REPLACED=0
INSTALL_DIR=""

try_install() {
  install_src=$1
  install_dir=$2
  if [ ! -d "$install_dir" ]; then
    mkdir -p "$install_dir" 2>/dev/null || return 1
  fi
  install_tmp=$(mktemp "$install_dir/.$BIN_NAME.XXXXXX" 2>/dev/null) || return 1
  if cp "$install_src" "$install_tmp" 2>/dev/null && chmod 0755 "$install_tmp" 2>/dev/null; then
    if [ -e "$install_dir/$BIN_NAME" ]; then
      was_replaced=1
    else
      was_replaced=0
    fi
    if mv -f "$install_tmp" "$install_dir/$BIN_NAME" 2>/dev/null; then
      REPLACED=$was_replaced
      return 0
    fi
  fi
  rm -f "$install_tmp" 2>/dev/null || true
  return 1
}

if [ -n "$EXISTING_PATH" ]; then
  existing_dir=$(dirname "$EXISTING_PATH")
  if try_install "$BIN_PATH" "$existing_dir"; then
    INSTALL_DIR=$existing_dir
  else
    die "could not replace existing installation at $EXISTING_PATH; re-run this script with sudo if $existing_dir requires elevated permissions"
  fi
elif try_install "$BIN_PATH" "$SYSTEM_DIR"; then
  INSTALL_DIR=$SYSTEM_DIR
elif try_install "$BIN_PATH" "$HOME/.local/bin"; then
  INSTALL_DIR="$HOME/.local/bin"
  say "Could not write to $SYSTEM_DIR; using $INSTALL_DIR instead."
else
  die "could not install $BIN_NAME to $SYSTEM_DIR or $HOME/.local/bin"
fi

installed="$INSTALL_DIR/$BIN_NAME"

in_path=0
case ":${PATH:-}:" in
  *":$INSTALL_DIR:"*) in_path=1 ;;
esac

existing=$(command -v "$BIN_NAME" 2>/dev/null || true)
shadowed=0
if [ -n "$existing" ] && [ "$existing" != "$installed" ]; then
  shadowed=1
fi

if [ -n "$VERSION" ]; then
  version_text="DFRelay v$VERSION"
else
  reported_version=$("$installed" --version 2>/dev/null || true)
  if [ -n "$reported_version" ]; then
    version_text=$reported_version
  else
    version_text="DFRelay"
  fi
fi

if [ "$REPLACED" -eq 1 ]; then
  say "Upgraded to $version_text"
else
  say "Installed $version_text"
fi
say "Location: $installed"
if [ "$in_path" -eq 1 ] && [ "$shadowed" -eq 0 ]; then
  say "Start: $BIN_NAME"
else
  say "Start: $installed"
fi
if [ "$REPLACED" -eq 1 ]; then
  say "Restart any running dfr daemon to use the new version."
fi
if [ "$in_path" -eq 0 ]; then
  warn "$INSTALL_DIR is not in PATH"
  say "Add it to your shell profile:"
  say "  export PATH=\"$INSTALL_DIR:\$PATH\""
fi
if [ "$shadowed" -eq 1 ]; then
  warn "another $BIN_NAME is on PATH at $existing; use $installed to run this version"
fi
if [ -n "$EXISTING_PATHS" ]; then
  previous_ifs=$IFS
  IFS='|'
  for other_path in $EXISTING_PATHS; do
    if [ "$other_path" != "$installed" ]; then
      warn "another DFRelay installation remains at $other_path"
    fi
  done
  IFS=$previous_ifs
fi
