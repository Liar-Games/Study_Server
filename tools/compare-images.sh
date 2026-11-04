#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
network="study-paired-$$"
server="$network-server"
current_image="${CURRENT_IMAGE:-study-engine-benchmark:local}"
baseline_image="${BASELINE_IMAGE:?set an existing baseline test image}"
results="${RESULT_DIR:-target/benchmarks/compare}"
mkdir -p "$results"
read -r -a modes <<< "${MODES:-echo broadcast}"
read -r -a counts <<< "${CLIENT_COUNTS:-64 256 1024}"
server_options=()
load_options=()
if [[ -n "${SERVER_CPUSET:-}" ]]; then server_options+=(--cpuset-cpus "$SERVER_CPUSET"); fi
if [[ -n "${LOAD_CPUSET:-}" ]]; then load_options+=(--cpuset-cpus "$LOAD_CPUSET"); fi
if [[ -n "${LOAD_CPUS:-}" ]]; then load_options+=(--cpus "$LOAD_CPUS"); fi
cleanup(){ docker rm -f "$server" >/dev/null 2>&1 || true; docker network rm "$network" >/dev/null 2>&1 || true; }
trap cleanup EXIT
docker network create "$network" >/dev/null
for mode in "${modes[@]}"; do
  for clients in "${counts[@]}"; do
    for run in 1 2 3; do
      for label in baseline current; do
        image="$baseline_image"
        if [[ "$label" == current ]]; then image="$current_image"; fi
        docker run -d --name "$server" --network "$network" --cpus 2 --memory 512m "${server_options[@]}" -e TOKIO_WORKER_THREADS=2 -e MESSAGES_PER_SECOND=100000 -e ROOM_MAILBOX_CAPACITY=2048 -e OUTBOUND_CAPACITY=1024 "$image" /app/target/release/study_server >/dev/null
        for attempt in {1..80}; do
          if docker exec "$server" bash -c 'echo > /dev/tcp/127.0.0.1/8080' 2>/dev/null; then break; fi
          sleep 0.1
        done
        docker run --rm --network "$network" "${load_options[@]}" -e TOKIO_WORKER_THREADS=4 "$current_image" /app/target/release/examples/load "ws://$server:8080/ws" "$clients" "${ROUNDS:-400}" 64 "$mode" 4 8 > "$results/$label-$mode-$clients-$run.json"
        docker exec "$server" cat /sys/fs/cgroup/cpu.stat /sys/fs/cgroup/memory.peak /sys/fs/cgroup/memory.events > "$results/$label-$mode-$clients-$run.resources.txt"
        cat "$results/$label-$mode-$clients-$run.json"
        docker rm -f "$server" >/dev/null
      done
    done
  done
done
