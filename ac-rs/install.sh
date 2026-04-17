#!/bin/bash
set -e

cd "$(dirname "$0")"

cargo build --release

sudo install -m 755 \
    target/release/ac \
    target/release/ac-daemon \
    target/release/ac-ui \
    /usr/local/bin/

echo "Installed: ac, ac-daemon, ac-ui → /usr/local/bin/"
