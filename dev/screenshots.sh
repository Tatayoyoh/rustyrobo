#!/bin/sh
# Regenerates docs/screenshots with the autotest walkthrough (src/autotest.rs):
# Rustyrobo opens on the desktop for about a minute. Its data is invented, on
# the servers of dev/docker-compose.yml, which must be up:
#   dev/ssh/gen-keys.sh && docker compose -f dev/docker-compose.yml up -d --wait
set -eu
cd "$(dirname "$0")/.."
config=$(mktemp -d)
trap 'rm -rf "$config"' EXIT
for seed in dev/screenshots/seed.js dev/demo-video/seed.js; do
  docker exec -i rr-mongo8 mongosh -u admin -p secret --quiet < "$seed" >/dev/null
done
# The connection editor shows the third one; the walkthrough connects to the first.
cat > "$config/settings.json" <<'EOF'
{
  "connections": [
    {
      "name": "Local Mongo 8", "group": "Docker", "color": [59, 130, 246],
      "hosts": [{ "host": "127.0.0.1", "port": 27117 }],
      "credentials": { "enabled": true, "database": "admin", "username": "admin", "password": "secret" }
    },
    { "name": "Legacy 4.2", "group": "Docker", "hosts": [{ "host": "127.0.0.1", "port": 27142 }] },
    {
      "name": "Mongo 8 via SSH", "group": "Docker", "color": [70, 167, 88],
      "hosts": [{ "host": "rr-mongo8", "port": 27017 }],
      "credentials": { "enabled": true, "database": "admin", "username": "admin", "password": "secret" },
      "ssh": { "enabled": true, "host": "127.0.0.1", "port": 2222, "username": "tunnel", "method": "Password", "password": "tunnelpw" }
    }
  ]
}
EOF
RUSTYROBO_CONFIG_DIR="$config" RUSTYROBO_AUTOTEST=docs/screenshots cargo run
