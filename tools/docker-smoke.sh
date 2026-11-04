#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
image="${1:-study-engine}"
network="study-engine-smoke-$$"
server="$network-server"
client="$network-client"
cleanup() {
  docker rm -f "$client" "$server" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
}
trap cleanup EXIT
docker network create "$network" >/dev/null
server_options=()
if [[ -n "${SMOKE_ENV_FILE:-}" ]]; then server_options+=(--env-file "$SMOKE_ENV_FILE"); fi
docker run -d --name "$server" --network "$network" "${server_options[@]}" "$image" >/dev/null
docker run -d --name "$client" --network "$network" node:22-alpine \
  node -e "$(cat tools/smoke-client.mjs)" "http://$server:8080" >/dev/null
ready=false
for attempt in {1..100}; do
  if [[ "$(docker logs "$client" 2>&1)" == *READY* ]]; then ready=true; break; fi
  sleep 0.1
done
if [[ "$ready" != true ]]; then docker logs "$client"; exit 1; fi
docker stop -t 10 "$server" >/dev/null
client_status="$(docker wait "$client")"
server_status="$(docker inspect "$server" --format '{{.State.ExitCode}}')"
docker logs "$client"
[[ "$client_status" == 0 && "$server_status" == 0 ]]
printf 'health endpoints, real WebSocket authentication, SIGTERM close, server exit: PASS\n'
