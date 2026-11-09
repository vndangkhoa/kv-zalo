#!/bin/zsh
# Builds a distributable Olaz: Developer ID signed, hardened runtime, notarized and stapled.
# Produces dist/Olaz-<version>.dmg (drag-to-Applications image) and dist/Olaz-<version>.zip (the stapled app).
#
#   ./scripts/release.sh                 # version 1.0, profile "olaz-notary"
#   OLAZ_VERSION=1.1 ./scripts/release.sh
#
# One-time setup (interactive, asks for an app-specific password from appleid.apple.com):
#   xcrun notarytool store-credentials "olaz-notary" --apple-id <apple-id> --team-id <team-id>
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${OLAZ_VERSION:-1.0}"
IDENTITY="${OLAZ_SIGN_IDENTITY:-Developer ID Application}"   # codesign matches the identity by substring
PROFILE="${OLAZ_NOTARY_PROFILE:-olaz-notary}"
APP="build/Olaz.app"
DIST="dist"
ZIP="$DIST/Olaz-$VERSION.zip"
DMG="$DIST/Olaz-$VERSION.dmg"

if ! security find-identity -v -p codesigning | grep -q "$IDENTITY"; then
    echo "error: no code-signing identity matching '$IDENTITY' in the keychain" >&2
    exit 2
fi

echo "==> Building and signing $APP ($VERSION)"
OLAZ_SIGN_IDENTITY="$IDENTITY" OLAZ_VERSION="$VERSION" ./scripts/make-app.sh release
codesign --verify --deep --strict --verbose=2 "$APP"
codesign -d --entitlements - "$APP" 2>/dev/null | grep -q allow-jit || { echo "error: JIT entitlement missing" >&2; exit 1; }

mkdir -p "$DIST"
rm -f "$ZIP" "$DMG"

if ! xcrun notarytool history --keychain-profile "$PROFILE" >/dev/null 2>&1; then
    echo "" >&2
    echo "Signed app is at $APP, but notarization needs a keychain profile named '$PROFILE'. Store one with:" >&2
    echo "  xcrun notarytool store-credentials \"$PROFILE\" --apple-id <apple-id> --team-id <team-id>" >&2
    echo "then re-run this script." >&2
    exit 2
fi

# Submits one file and fails loudly (with Apple's log) unless the ticket was issued.
notarize() {
    local file="$1" out id
    echo "==> Notarizing $file"
    out="$(xcrun notarytool submit "$file" --keychain-profile "$PROFILE" --wait 2>&1)" || true
    echo "$out"
    if ! grep -q 'status: Accepted' <<< "$out"; then
        id="$(grep -m1 -E '^\s*id:' <<< "$out" | awk '{print $2}')"
        [[ -n "$id" ]] && xcrun notarytool log "$id" --keychain-profile "$PROFILE" >&2 || true
        echo "error: notarization of $file was not accepted" >&2
        exit 1
    fi
}

# 1. The app itself, so the stapled ticket travels with it even if someone copies it out of the DMG.
ditto -c -k --keepParent "$APP" "$ZIP"
notarize "$ZIP"
xcrun stapler staple "$APP"
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"   # re-zip the stapled app

# 2. A drag-to-Applications DMG, signed, notarized and stapled too.
echo "==> Creating $DMG"
STAGE="$(mktemp -d)"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "Olaz" -srcfolder "$STAGE" -ov -quiet -format UDZO "$DMG"
rm -rf "$STAGE"
codesign --force --timestamp --sign "$IDENTITY" "$DMG"
notarize "$DMG"
xcrun stapler staple "$DMG"

echo "==> Gatekeeper assessment"
spctl --assess --type execute -vv "$APP"
spctl --assess --type open --context context:primary-signature -vv "$DMG"
xcrun stapler validate "$APP"
xcrun stapler validate "$DMG"

echo ""
echo "Release $VERSION ready:"
ls -la "$DIST"
shasum -a 256 "$DMG" "$ZIP"
