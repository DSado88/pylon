#!/bin/bash
set -e
cargo build --release
mkdir -p "Pylon.app/Contents/MacOS"
cp "Claude Cockpit.app/Contents/Info.plist" "Pylon.app/Contents/"
cp target/release/pylon "Pylon.app/Contents/MacOS/pylon"
# Remove old app if present
rm -rf "/Applications/Claude Cockpit.app"
cp -r "Pylon.app" /Applications/
echo "Installed Pylon.app to /Applications"
