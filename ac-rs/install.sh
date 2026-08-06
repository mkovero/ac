#!/bin/bash
set -e

cd "$(dirname "$0")"

cargo build --release

sudo install -m 755 \
    target/release/ac \
    target/release/ac-daemon \
    target/release/ac-view \
    /usr/local/bin/

echo "Installed: ac, ac-daemon, ac-view → /usr/local/bin/"
sha256sum /usr/local/bin/ac /usr/local/bin/ac-daemon /usr/local/bin/ac-view
