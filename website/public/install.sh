#!/bin/sh
# Installs Kargo, Cargo for Kotlin:
#
#   curl -fsSL https://enekocm7.github.io/kargo/install.sh | sh
#
# Options (pass them with `sh -s -- <option>` when piping):
#   --version <version>   install this version instead of the latest (or set KARGO_VERSION)
#   --no-modify-path      do not edit shell startup files to add ~/.kargo/bin to PATH
#   -y, --yes             edit the startup files without asking
#   -h, --help            show this help
#
# Environment:
#   KARGO_VERSION         the version to install
#   KARGO_HOME            where Kargo lives (default ~/.kargo); the binary goes to $KARGO_HOME/bin
#   KARGO_RELEASES_URL    where releases are published (default https://github.com/enekocm7/kargo/releases;
#                         a mirror, or a directory with file://)
#   KARGO_TARGET          install the build for this target instead of the one detected, e.g. x86_64-unknown-linux-musl
#
# The archive's SHA-256 is checked against the release's SHA256SUMS before anything is installed. On Windows use
# install.ps1.
set -eu

RELEASES_URL=${KARGO_RELEASES_URL:-https://github.com/enekocm7/kargo/releases}
RELEASES_URL=${RELEASES_URL%/}
VERSION=${KARGO_VERSION:-}
MODIFY_PATH=ask

say() {
  printf 'kargo-install: %s\n' "$*" >&2
}

fail() {
  say "error: $*"
  exit 1
}

usage() {
  cat << 'USAGE'
Installs Kargo, Cargo for Kotlin.

Usage: install.sh [options]

  --version <version>   install this version instead of the latest (or set KARGO_VERSION)
  --no-modify-path      do not edit shell startup files to add ~/.kargo/bin to PATH
  -y, --yes             edit the startup files without asking
  -h, --help            show this help

Environment: KARGO_VERSION, KARGO_HOME (default ~/.kargo), KARGO_RELEASES_URL (a mirror, or a directory with file://),
KARGO_TARGET (install the build for this target instead of the detected one).
USAGE
}

have() {
  command -v "$1" > /dev/null 2>&1
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      [ "$#" -ge 2 ] || fail "--version needs a value"
      VERSION=$2
      shift
      ;;
    --version=*) VERSION=${1#--version=} ;;
    --no-modify-path) MODIFY_PATH=no ;;
    -y | --yes) MODIFY_PATH=yes ;;
    -h | --help)
      usage
      exit 0
      ;;
    *) fail "unknown option: $1 (try --help)" ;;
  esac
  shift
done
VERSION=${VERSION#v}

if have curl; then
  fetch() { curl --proto '=https,file' --tlsv1.2 -fsSL "$1" -o "$2"; }
elif have wget; then
  fetch() { wget -q -O "$2" "$1"; }
else
  fail "curl or wget is needed to download Kargo"
fi

# The target triple of the build to install.
detect_target() {
  if [ -n "${KARGO_TARGET:-}" ]; then
    printf '%s' "$KARGO_TARGET"
    return
  fi
  os=$(uname -s)
  arch=$(uname -m)
  case "$arch" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) fail "no prebuilt Kargo for the $arch architecture; build it from source: https://enekocm7.github.io/kargo/getting-started/installation/" ;;
  esac
  case "$os" in
    Darwin)
      # A shell running under Rosetta reports x86_64 on an Apple Silicon Mac.
      if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2> /dev/null || true)" = 1 ]; then
        arch=aarch64
      fi
      printf '%s-apple-darwin' "$arch"
      ;;
    Linux)
      # glibc 2.35 is what the gnu builds need; anything else (Alpine, older distributions) gets the static musl build.
      libc=musl
      if have getconf; then
        glibc=$(getconf GNU_LIBC_VERSION 2> /dev/null || true)
        glibc=${glibc#glibc }
        major=${glibc%%.*}
        minor=${glibc#*.}
        minor=${minor%%.*}
        case "$major$minor" in
          '' | *[!0-9]*) ;;
          *)
            if [ "$major" -gt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -ge 35 ]; }; then
              libc=gnu
            fi
            ;;
        esac
      fi
      if [ "$libc" = gnu ]; then
        printf '%s-unknown-linux-gnu' "$arch"
      else
        printf '%s-unknown-linux-musl' "$arch"
      fi
      ;;
    *) fail "this script installs Kargo on Linux and macOS; on Windows run install.ps1: https://enekocm7.github.io/kargo/getting-started/installation/" ;;
  esac
}

sha256_of() {
  if have sha256sum; then
    sha256sum "$1" | cut -d ' ' -f 1
  elif have shasum; then
    shasum -a 256 "$1" | cut -d ' ' -f 1
  elif have openssl; then
    openssl dgst -sha256 "$1" | sed 's/^.*= //'
  else
    fail "sha256sum, shasum or openssl is needed to verify the download"
  fi
}

TARGET=$(detect_target)
KARGO_HOME_DIR=${KARGO_HOME:-$HOME/.kargo}
BIN_DIR=$KARGO_HOME_DIR/bin

tmp=$(mktemp -d 2> /dev/null || mktemp -d -t kargo-install)
trap 'rm -rf "$tmp"' EXIT INT TERM

if [ -z "$VERSION" ]; then
  say "looking up the latest release"
  fetch "$RELEASES_URL/latest/download/VERSION" "$tmp/VERSION" || fail "could not find the latest release at $RELEASES_URL"
  VERSION=$(tr -d '[:space:]' < "$tmp/VERSION")
  VERSION=${VERSION#v}
fi
[ -n "$VERSION" ] || fail "the latest release has no version"

ARCHIVE=kargo-$VERSION-$TARGET.tar.gz
BASE=$RELEASES_URL/download/v$VERSION

say "installing kargo $VERSION for $TARGET"
fetch "$BASE/SHA256SUMS" "$tmp/SHA256SUMS" || fail "could not find release v$VERSION at $BASE"
expected=$(awk -v f="$ARCHIVE" '{ n = $2; sub(/^\*/, "", n) } n == f { print tolower($1) }' "$tmp/SHA256SUMS")
[ -n "$expected" ] || fail "release v$VERSION has no build for $TARGET ($ARCHIVE)"

fetch "$BASE/$ARCHIVE" "$tmp/$ARCHIVE" || fail "could not download $BASE/$ARCHIVE"
actual=$(sha256_of "$tmp/$ARCHIVE")
if [ "$actual" != "$expected" ]; then
  fail "checksum mismatch for $ARCHIVE: expected $expected, got $actual"
fi

mkdir "$tmp/unpacked"
tar -xzf "$tmp/$ARCHIVE" -C "$tmp/unpacked"
binary=$tmp/unpacked/kargo-$VERSION-$TARGET/kargo
[ -f "$binary" ] || binary=$tmp/unpacked/kargo
[ -f "$binary" ] || fail "the archive does not contain the kargo binary"

mkdir -p "$BIN_DIR"
# Copy next to the destination and rename, so a running kargo is replaced atomically.
cp "$binary" "$BIN_DIR/.kargo-install.$$"
chmod 755 "$BIN_DIR/.kargo-install.$$"
mv -f "$BIN_DIR/.kargo-install.$$" "$BIN_DIR/kargo"
say "installed $BIN_DIR/kargo"

# The line the startup files get: the default home is written as $HOME/.kargo so the files keep working if the home moves.
if [ "$BIN_DIR" = "$HOME/.kargo/bin" ]; then
  # shellcheck disable=SC2016
  PATH_ENTRY='$HOME/.kargo/bin'
else
  PATH_ENTRY=$BIN_DIR
fi

in_path() {
  case ":$PATH:" in
    *":$BIN_DIR:"*) return 0 ;;
    *) return 1 ;;
  esac
}

# Appends the PATH block to $1 unless it is there already.
add_block() {
  file=$1
  syntax=$2
  if [ -f "$file" ] && grep -q '^# >>> kargo >>>$' "$file"; then
    return
  fi
  mkdir -p "$(dirname "$file")"
  {
    printf '\n# >>> kargo >>>\n'
    if [ "$syntax" = fish ]; then
      printf 'fish_add_path -g "%s"\n' "$PATH_ENTRY"
    else
      # shellcheck disable=SC2016
      printf 'export PATH="%s:$PATH"\n' "$PATH_ENTRY"
    fi
    printf '# <<< kargo <<<\n'
  } >> "$file"
  say "added $BIN_DIR to PATH in $file"
}

modify_path() {
  add_block "$HOME/.profile" sh
  [ -f "$HOME/.bashrc" ] && add_block "$HOME/.bashrc" sh
  [ -f "$HOME/.bash_profile" ] && add_block "$HOME/.bash_profile" sh
  if have zsh || [ -f "${ZDOTDIR:-$HOME}/.zshenv" ]; then
    add_block "${ZDOTDIR:-$HOME}/.zshenv" sh
  fi
  if have fish; then
    add_block "${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/kargo.fish" fish
  fi
  return 0
}

if in_path; then
  :
elif [ "$MODIFY_PATH" = no ]; then
  say "$BIN_DIR is not in your PATH; add it with: export PATH=\"$BIN_DIR:\$PATH\""
else
  answer=yes
  if [ "$MODIFY_PATH" = ask ]; then
    # The script itself comes through stdin when piped from curl, so the question goes to the terminal.
    if ( : < /dev/tty ) 2> /dev/null; then
      printf 'kargo-install: add %s to your PATH by editing your shell startup files? [Y/n] ' "$BIN_DIR" >&2
      read -r reply < /dev/tty || reply=
      case "$reply" in
        n | N | no | No | NO) answer=no ;;
      esac
    else
      answer=no
      say "no terminal to ask on, so your shell startup files stay as they are (use --yes to edit them)"
    fi
  fi
  if [ "$answer" = yes ]; then
    modify_path
    say "restart your shell, or run: export PATH=\"$BIN_DIR:\$PATH\""
  else
    say "add $BIN_DIR to your PATH: export PATH=\"$BIN_DIR:\$PATH\""
  fi
fi

"$BIN_DIR/kargo" --version >&2 || say "warning: the installed kargo did not start"
say "next: kargo new hello && cd hello && kargo run"
