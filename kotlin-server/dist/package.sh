#!/bin/sh
# Builds the server's distribution, target/dist/kargo-kotlin-server-<version>.zip, with Kargo:
#
#   lib/          the server's jar and every jar of its runtime classpath
#   bin/          the launchers (kargo lsp runs `java -cp lib/*` itself)
#   NOTICE        the bundled components and their licenses
#   version.json  the server's version and the Kotlin frontend it analyzes with
#
# KARGO names the kargo executable (default: `kargo` on PATH). Prints the zip's SHA-256.
set -eu
cd "$(dirname "$0")/.."
kargo=${KARGO:-kargo}
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' kargo.toml | head -n 1)
name="kargo-kotlin-server-$version"
out="target/dist/$name"

"$kargo" build --release
rm -rf "$out" "target/dist/$name.zip"
mkdir -p "$out/lib" "$out/bin"
jar --create --file "$out/lib/$name.jar" -C target/release/jvm/classes .
"$kargo" -q classpath --scope runtime --format lines | tr -d '\r' | while IFS= read -r entry; do
    cp "$entry" "$out/lib/"
done
cp dist/bin/kargo-kotlin-server dist/bin/kargo-kotlin-server.bat "$out/bin/"
chmod +x "$out/bin/kargo-kotlin-server"
cp NOTICE "$out/"
kotlin=$(unzip -p "$out"/lib/symbol-processing-aa-*.jar META-INF/compiler.version 2>/dev/null || echo unknown)
printf '{ "name": "kargo-kotlin-server", "version": "%s", "kotlinFrontend": "%s" }\n' "$version" "$kotlin" > "$out/version.json"

cd target/dist
if command -v zip > /dev/null; then
    zip -qr "$name.zip" "$name"
else
    jar --create --no-manifest --file "$name.zip" "$name"
fi
if command -v sha256sum > /dev/null; then sha256sum "$name.zip"; else shasum -a 256 "$name.zip"; fi
