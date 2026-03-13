#!/bin/bash
set -e
cargo build --release
cp target/release/claude-cockpit "Claude Cockpit.app/Contents/MacOS/claude-cockpit"
cp -r "Claude Cockpit.app" /Applications/
echo "Installed Claude Cockpit.app to /Applications"
