#!/bin/bash
cd "$(dirname "$0")"
export PATH="$PWD/target/debug:$PATH"
echo "Flick shell — Ready."
exec bash
