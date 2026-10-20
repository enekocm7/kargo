#!/usr/bin/env bash
# Fills the package manager templates in packaging/ for a release.
#
#   packaging/render.sh <version> <SHA256SUMS> <output-dir>
#
# The release workflow runs it after building and uploads the output as the `packages` artifact. Each template names
# the archives it needs (kargo-<version>-<target>.<ext>) and `@SHA_<target>@` is that archive's checksum, with the
# dashes of the target turned into underscores (`@SHA_UPPER_<target>@` is the same in capitals).
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <version> <SHA256SUMS> <output-dir>" >&2
  exit 2
fi
version=$1
sums=$2
out=$3
here=$(cd "$(dirname "$0")" && pwd)

# 0.3.0-rc.1 is not a valid Arch pkgver.
pkgver=${version//-/_}

targets=(
  x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
  x86_64-unknown-linux-musl aarch64-unknown-linux-musl
  x86_64-apple-darwin aarch64-apple-darwin
  x86_64-pc-windows-msvc aarch64-pc-windows-msvc
)

sed_script=$(mktemp)
trap 'rm -f "$sed_script"' EXIT
{
  printf 's|@VERSION@|%s|g\n' "$version"
  printf 's|@PKGVER@|%s|g\n' "$pkgver"
  for target in "${targets[@]}"; do
    case "$target" in
      *windows*) ext=zip ;;
      *) ext=tar.gz ;;
    esac
    file="kargo-${version}-${target}.${ext}"
    sha=$(awk -v f="$file" '{ n = $2; sub(/^\*/, "", n) } n == f { print tolower($1) }' "$sums")
    if [ -z "$sha" ]; then
      echo "no checksum for $file in $sums" >&2
      exit 1
    fi
    key=${target//-/_}
    printf 's|@SHA_%s@|%s|g\n' "$key" "$sha"
    printf 's|@SHA_UPPER_%s@|%s|g\n' "$key" "$(echo "$sha" | tr '[:lower:]' '[:upper:]')"
  done
} > "$sed_script"

mkdir -p "$out/winget"
sed -f "$sed_script" "$here/homebrew/kargo.rb.in" > "$out/kargo.rb"
sed -f "$sed_script" "$here/scoop/kargo.json.in" > "$out/kargo.json"
sed -f "$sed_script" "$here/aur/PKGBUILD.in" > "$out/PKGBUILD"
for template in "$here"/winget/*.yaml.in; do
  name=$(basename "$template" .in)
  sed -f "$sed_script" "$template" > "$out/winget/$name"
done

if grep -rnE '@[A-Za-z0-9_]+@' "$out" > /dev/null; then
  echo "unfilled placeholders:" >&2
  grep -rnE '@[A-Za-z0-9_]+@' "$out" >&2
  exit 1
fi
echo "wrote the package manager files for kargo $version to $out"
