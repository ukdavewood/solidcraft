#!/bin/bash
# Build the Rust binary for the device and put it where Xcode expects the app's executable;
# Xcode signs the bundle afterwards.
set -euo pipefail
SRC="$(cd "$(dirname "$0")/../../.." && pwd)"
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
cd "$SRC"
cargo build --release --target aarch64-apple-ios -p solidcraft-ios
mkdir -p "$TARGET_BUILD_DIR/$CONTENTS_FOLDER_PATH"
cp "$SRC/target/aarch64-apple-ios/release/solidcraft-ios" "$TARGET_BUILD_DIR/$EXECUTABLE_PATH"
