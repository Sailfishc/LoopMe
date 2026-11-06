#!/bin/bash
# Builds, signs, notarizes and publishes a Mac update for Sparkle.
# See docs/mac-updates.md. Run from anywhere on a Mac with Xcode, xcodegen and Rust.
#
# Put these in apple/release.env (git-ignored) or the environment.
# Required:
#   LOOPME_UPDATE_FEED_URL    where the app looks for updates, e.g. https://files.corp/loopme/appcast.xml
#   LOOPME_UPDATE_PUBLIC_KEY  Sparkle EdDSA public key (from generate_keys)
#   NOTARY_PROFILE            notarytool keychain profile (xcrun notarytool store-credentials)
# Optional:
#   DOWNLOAD_URL_PREFIX       where the zips are served (default: the feed URL's folder)
#   UPDATES_DIR               folder that keeps every released zip + appcast.xml (default apple/build/updates)
#   UPLOAD_CMD                run after the appcast is generated, with $UPDATES_DIR exported,
#                             e.g. 'ossutil cp -r "$UPDATES_DIR" oss://bucket/loopme/ -f'
#   RELEASE_NOTES             text shown in the update dialog
set -euo pipefail

cd "$(dirname "$0")/.."
APPLE="$PWD"
# Internal addresses and commands live in an untracked file (the repo is open source).
if [ -f "$APPLE/release.env" ]; then
  set -a; . "$APPLE/release.env"; set +a
fi
: "${LOOPME_UPDATE_FEED_URL:?set LOOPME_UPDATE_FEED_URL}"
: "${LOOPME_UPDATE_PUBLIC_KEY:?set LOOPME_UPDATE_PUBLIC_KEY}"
: "${NOTARY_PROFILE:?set NOTARY_PROFILE}"
DOWNLOAD_URL_PREFIX="${DOWNLOAD_URL_PREFIX:-${LOOPME_UPDATE_FEED_URL%/*}/}"
export UPDATES_DIR="${UPDATES_DIR:-$APPLE/build/updates}"

VERSION=$(sed -n 's/^ *MARKETING_VERSION: "\(.*\)"/\1/p' project.yml | head -1)
BUILD=$(sed -n 's/^ *CURRENT_PROJECT_VERSION: "\(.*\)"/\1/p' project.yml | head -1)
echo "==> LoopMe for Mac $VERSION ($BUILD)"

WORK="$APPLE/build/mac-release"
rm -rf "$WORK" && mkdir -p "$WORK" "$UPDATES_DIR"

xcodegen generate
xcodebuild -project LoopMe.xcodeproj -scheme LoopMeMac -configuration Release \
  -destination 'generic/platform=macOS' \
  -archivePath "$WORK/LoopMe.xcarchive" \
  -clonedSourcePackagesDirPath "$APPLE/build/SourcePackages" \
  -allowProvisioningUpdates \
  LOOPME_UPDATE_FEED_URL="$LOOPME_UPDATE_FEED_URL" \
  LOOPME_UPDATE_PUBLIC_KEY="$LOOPME_UPDATE_PUBLIC_KEY" \
  archive
xcodebuild -exportArchive -archivePath "$WORK/LoopMe.xcarchive" \
  -exportOptionsPlist ExportOptions-DeveloperID.plist \
  -exportPath "$WORK/export" -allowProvisioningUpdates

APP="$WORK/export/LoopMe.app"
ZIP="$WORK/LoopMe-$VERSION-$BUILD.zip"
echo "==> notarizing"
ditto -c -k --keepParent "$APP" "$ZIP"
xcrun notarytool submit "$ZIP" --keychain-profile "$NOTARY_PROFILE" --wait
xcrun stapler staple "$APP"
rm -f "$ZIP" && ditto -c -k --keepParent "$APP" "$ZIP"
cp "$ZIP" "$UPDATES_DIR/"
if [ -n "${RELEASE_NOTES:-}" ]; then
  printf '%s\n' "$RELEASE_NOTES" > "$UPDATES_DIR/LoopMe-$VERSION-$BUILD.txt"
fi

echo "==> appcast"
GEN=$(find "$APPLE/build/SourcePackages/artifacts" -path '*Sparkle*/bin/generate_appcast' -type f | head -1)
[ -x "$GEN" ] || { echo "generate_appcast not found under build/SourcePackages"; exit 1; }
# Signs each zip with the EdDSA private key in your login keychain (from generate_keys).
"$GEN" --download-url-prefix "$DOWNLOAD_URL_PREFIX" "$UPDATES_DIR"

if [ -n "${UPLOAD_CMD:-}" ]; then
  echo "==> upload"
  bash -c "$UPLOAD_CMD"
else
  echo "==> done. Upload $UPDATES_DIR/appcast.xml and $(basename "$ZIP") to ${DOWNLOAD_URL_PREFIX}"
fi
