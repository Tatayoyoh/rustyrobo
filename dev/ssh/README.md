Throw-away SSH keys for the live tests against the `rr-ssh` container.
Generate them with `./gen-keys.sh` before `docker compose up` (they are git-ignored).

- `test_key_plain`: no passphrase
- `test_key_encrypted`: passphrase `testpass`
