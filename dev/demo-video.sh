#!/bin/sh
# Records the demo video: Rustyrobo at 960x720, driven by the keyboard (the
# connection manager, Ctrl+P, Alt+arrows, Edit Document), the keys shown at the
# bottom. Its server is a throwaway MongoDB container with invented data.
# Needs Docker and ffmpeg. Usage: dev/demo-video.sh [out.mp4 | out.gif]
# (the README shows docs/demo.gif).
set -eu
cd "$(dirname "$0")/.."
out=$(realpath -m "${1:-demo.mp4}")
port=27217
work=$(mktemp -d)
docker run -d --rm --name rr-demo -p "$port:27017" mongo:8 >/dev/null
trap 'docker stop rr-demo >/dev/null; rm -rf "$work"' EXIT
until docker exec rr-demo mongosh --quiet --eval 'db.runCommand({ ping: 1 })' >/dev/null 2>&1; do sleep 1; done
docker exec -i rr-demo mongosh --quiet < dev/demo-video/seed.js >/dev/null
# The target of the app: its dependencies are built once.
RUSTYROBO_CONFIG_DIR="$work/config" cargo run --manifest-path dev/demo-video/Cargo.toml --target-dir target -- "$work/frames" "$port"
case "$out" in
# 15 frames a second, the colours of the frames: 1.5 MB, the text sharp.
*.gif) ffmpeg -loglevel error -y -framerate 30 -i "$work/frames/%05d.png" \
    -vf "fps=15,split[a][b];[a]palettegen=max_colors=128:stats_mode=diff[p];[b][p]paletteuse=dither=none:diff_mode=rectangle" "$out" ;;
*) ffmpeg -loglevel error -y -framerate 30 -i "$work/frames/%05d.png" -c:v libx264 -pix_fmt yuv420p -crf 18 -movflags +faststart "$out" ;;
esac
echo "$out"
