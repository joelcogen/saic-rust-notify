#!/bin/sh
# Offline smoke check: only synthetic settings; never connects to SAIC production.
set -eu
image=${1:-saic-rust-notify:local}
container_name=saic-rust-notify-smoke-$$
cleanup() { docker rm -fv "$container_name" >/dev/null 2>&1 || true; }
trap cleanup EXIT HUP INT TERM
docker run -d --name "$container_name" \
  --read-only --cap-drop ALL --security-opt no-new-privileges \
  -e SAIC_BASE_URL=https://127.0.0.1:9/ \
  -e SAIC_USERNAME=synthetic -e SAIC_PASSWORD=synthetic \
  -e CLIMATE_API_TOKEN=synthetic-smoke-token \
  -p 127.0.0.1::8080 "$image" >/dev/null
port=$(docker port "$container_name" 8080/tcp)
ready=false
for attempt in 1 2 3 4 5 6 7 8 9 10; do
  if curl --silent --fail --max-time 2 -H 'Authorization: Bearer synthetic-smoke-token' "http://$port/api/status" >/dev/null; then
    ready=true
    break
  fi
  sleep 1
done
[ "$ready" = true ]
[ "$(docker exec "$container_name" id -u)" = 10001 ]
[ "$(docker exec "$container_name" id -g)" = 10001 ]
docker exec "$container_name" sh -ec 'test -w /app/data && test -f /app/data/polling.json'
[ "$(curl --silent --output /dev/null --write-out '%{http_code}' "http://$port/api/status")" = 401 ]
for asset in / /dashboard.html /tabler-1.7.5.min.css /dashboard.css /dashboard.js /dashboard-model.mjs /car.jpg /favicon.svg; do
  [ "$(curl --silent --output /dev/null --write-out '%{http_code}' "http://$port$asset")" = 200 ]
done
docker exec "$container_name" sh -ec '
  for executable in python python3 pip cargo; do
    if command -v "$executable"; then exit 1; fi
  done
  test -z "$(find /app /usr/local -type f \( -name "*.py" -o -name "*.html" -o -name "*.css" -o -name "*.js" -o -name "runtime.env" \) -print)"
'
docker stop -t 5 "$container_name" >/dev/null
[ "$(docker inspect --format '{{.State.ExitCode}}' "$container_name")" = 0 ]
echo 'Container smoke check passed: non-root, authenticated API, embedded dashboard, no Python, graceful shutdown.'
