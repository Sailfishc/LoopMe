#!/bin/bash
# Builds, signs, notarizes and publishes a Mac update as a GitHub Release.
# See docs/mac-updates.md. Run on a Mac with Xcode, xcodegen, Rust and the GitHub CLI (gh, logged in).
#
# Settings come from the environment or apple/release.env (git-ignored):
#   LOOPME_UPDATE_PUBLIC_KEY  Sparkle EdDSA public key (from generate_keys); or set it in project.yml (Release)
#   NOTARY_PROFILE            notarytool keychain profile (xcrun notarytool store-credentials)
#   GITHUB_REPO               default Sailfishc/LoopMe
#   RELEASE_NOTES             text shown in the update dialog and on the release page
#   DRY_RUN=1                 build and notarize, but don't publish
set -euo pipefail

cd "$(dirname "$0")/.."
APPLE="$PWD"
if [ -f "$APPLE/release.env" ]; then
  set -a; . "$APPLE/release.env"; set +a
fi
: "${NOTARY_PROFILE:?set NOTARY_PROFILE}"
REPO="${GITHUB_REPO:-Sailfishc/LoopMe}"
# Every release carries appcast.xml; this URL always redirects to the newest one.
FEED_URL="https://github.com/$REPO/releases/latest/download/appcast.xml"

VERSION=$(sed -n 's/^ *MARKETING_VERSION: "\(.*\)"/\1/p' project.yml | head -1)
BUILD=$(sed -n 's/^ *CURRENT_PROJECT_VERSION: "\(.*\)"/\1/p' project.yml | head -1)
TAG="mac-v$VERSION-$BUILD"
NAME="LoopMe-$VERSION-$BUILD"
echo "==> LoopMe for Mac $VERSION ($BUILD) → $REPO@$TAG"

if [ -z "${DRY_RUN:-}" ] && gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1; then
  echo "release $TAG already exists: raise CURRENT_PROJECT_VERSION in project.yml"; exit 1
fi

WORK="$APPLE/build/mac-release"
rm -rf "$WORK" && mkdir -p "$WORK/feed"

xcodegen generate
EXTRA=(LOOPME_UPDATE_FEED_URL="$FEED_URL")
[ -n "${LOOPME_UPDATE_PUBLIC_KEY:-}" ] && EXTRA+=(LOOPME_UPDATE_PUBLIC_KEY="$LOOPME_UPDATE_PUBLIC_KEY")
xcodebuild -project LoopMe.xcodeproj -scheme LoopMeMac -configuration Release \
  -destination 'generic/platform=macOS' \
  -archivePath "$WORK/LoopMe.xcarchive" \
  -clonedSourcePackagesDirPath "$APPLE/build/SourcePackages" \
  -allowProvisioningUpdates "${EXTRA[@]}" archive
xcodebuild -exportArchive -archivePath "$WORK/LoopMe.xcarchive" \
  -exportOptionsPlist ExportOptions-DeveloperID.plist \
  -exportPath "$WORK/export" -allowProvisioningUpdates

APP="$WORK/export/LoopMe.app"
KEY=$(/usr/libexec/PlistBuddy -c 'Print :SUPublicEDKey' "$APP/Contents/Info.plist" 2>/dev/null || true)
[ -n "$KEY" ] || { echo "SUPublicEDKey is empty: set LOOPME_UPDATE_PUBLIC_KEY"; exit 1; }

echo "==> notarizing"
ZIP="$WORK/feed/$NAME.zip"
ditto -c -k --keepParent "$APP" "$ZIP"
xcrun notarytool submit "$ZIP" --keychain-profile "$NOTARY_PROFILE" --wait
xcrun stapler staple "$APP"
rm -f "$ZIP" && ditto -c -k --keepParent "$APP" "$ZIP"
NOTES="${RELEASE_NOTES:-LoopMe for Mac $VERSION ($BUILD)}"
printf '%s\n' "$NOTES" > "$WORK/feed/$NAME.txt"

echo "==> appcast"
GEN=$(find "$APPLE/build/SourcePackages/artifacts" -path '*Sparkle*/bin/generate_appcast' -type f | head -1)
[ -x "$GEN" ] || { echo "generate_appcast not found under build/SourcePackages"; exit 1; }
# One item per appcast: the zip of this release (small app, no deltas needed).
# Signed with the EdDSA private key in your login keychain (from generate_keys).
"$GEN" --download-url-prefix "https://github.com/$REPO/releases/download/$TAG/" "$WORK/feed"

if [ -n "${DRY_RUN:-}" ]; then
  echo "==> dry run: $WORK/feed"; ls -l "$WORK/feed"; exit 0
fi
echo "==> publishing"
# Upload as a draft first, then publish: "latest" must never point at a release without its appcast.
gh release create "$TAG" --repo "$REPO" --draft --title "LoopMe for Mac $VERSION ($BUILD)" --notes "$NOTES" \
  "$ZIP" "$WORK/feed/appcast.xml"
gh release edit "$TAG" --repo "$REPO" --draft=false --latest
echo "==> done: $FEED_URL now points at $TAG"
