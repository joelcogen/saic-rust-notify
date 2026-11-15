#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
  echo "Usage: $0 user@host" >&2
  exit 2
fi

target=$1
image=saic-rust-notify:deploy
container=saic-rust-notify
host_dir=saic-rust-notify
archive_name=saic-rust-notify-image.tar

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

cd "$repo_dir"
DOCKER_DEFAULT_PLATFORM=linux/amd64 ./scripts/build-container.sh "$image"
docker save "$image" -o "$tmp_dir/$archive_name"

# Ensure the host-side configuration directory exists, then transfer the image.
ssh "$target" "mkdir -p '$host_dir'"
scp "$tmp_dir/$archive_name" "$target:$host_dir/$archive_name"

ssh "$target" sh -s -- "$host_dir" "$archive_name" "$image" "$container" <<'REMOTE'
set -eu
host_dir=$1
archive_name=$2
image=$3
container=$4

if [ ! -f "$host_dir/.env" ]; then
  echo "Missing $HOME/$host_dir/.env; create it on the server before deploying." >&2
  exit 1
fi

docker load -i "$host_dir/$archive_name"
rm -f "$host_dir/$archive_name"
docker volume create saic-rust-notify-data >/dev/null
docker rm -f "$container" >/dev/null 2>&1 || true
docker run -d \
  --name "$container" \
  --restart unless-stopped \
  --env-file "$host_dir/.env" \
  --publish 127.0.0.1:8080:8080 \
  --mount type=volume,src=saic-rust-notify-data,dst=/app/data \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  "$image"
REMOTE

echo "Deployed $image to $target"
