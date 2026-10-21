#!/bin/sh
# Generates the throw-away SSH keys used by the live tests (run before
# `docker compose up`). Keys are git-ignored and grant access to nothing but
# the rr-ssh test container.
set -e
cd "$(dirname "$0")"
mkdir -p pubkeys
[ -f test_key_plain ] || ssh-keygen -q -t ed25519 -N "" -C rustyrobo-test-plain -f test_key_plain
[ -f test_key_encrypted ] || ssh-keygen -q -t ed25519 -N testpass -C rustyrobo-test-encrypted -f test_key_encrypted
cp test_key_plain.pub test_key_encrypted.pub pubkeys/
