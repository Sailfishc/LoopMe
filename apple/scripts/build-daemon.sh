#!/bin/bash
# Xcode build phase for LoopMeMac: builds the Rust daemon and puts it in
# LoopMe.app/Contents/MacOS/loopme-daemon, signed with the app's identity.
set -euo pipefail

export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
DAEMON_DIR="${SRCROOT}/../daemon"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found. Install Rust with: curl https://sh.rustup.rs -sSf | sh"
  exit 1
fi

if [ "${CONFIGURATION}" = "Release" ]; then
  PROFILE=release
  cargo build --manifest-path "${DAEMON_DIR}/Cargo.toml" --release
else
  PROFILE=debug
  cargo build --manifest-path "${DAEMON_DIR}/Cargo.toml"
fi

DEST="${TARGET_BUILD_DIR}/${EXECUTABLE_FOLDER_PATH}"
mkdir -p "${DEST}"
cp -f "${DAEMON_DIR}/target/${PROFILE}/loopme-daemon" "${DEST}/loopme-daemon"

# Sign the helper before Xcode signs the app bundle (hardened runtime is required for notarization).
if [ -n "${EXPANDED_CODE_SIGN_IDENTITY:-}" ] && [ "${EXPANDED_CODE_SIGN_IDENTITY}" != "-" ]; then
  if [ "${CONFIGURATION}" = "Release" ]; then TS="--timestamp"; else TS="--timestamp=none"; fi
  codesign --force --options runtime ${TS} --sign "${EXPANDED_CODE_SIGN_IDENTITY}" "${DEST}/loopme-daemon"
else
  codesign --force --sign - "${DEST}/loopme-daemon"
fi
