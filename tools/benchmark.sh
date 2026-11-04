#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Uses only containers/network created by this invocation.
network="study-engine-benchmark-$$"
server="${network}-server"
image="study-engine-benchmark:local"
results="${RESULT_DIR:-target/benchmarks/local}"
mkdir -p "$results"
cleanup() {
  docker rm -f "$server" >/dev/null 2>&1 || true
  docker network rm "$network" >/dev/null 2>&1 || true
}
trap cleanup EXIT
docker build --target test -t "$image" .
docker network create "$network" >/dev/null
docker run -d --name "$server" --network "$network" --cpus 2 --memory 512m \
  -e TOKIO_WORKER_THREADS=2 -e MESSAGES_PER_SECOND=100000 \
  -e ROOM_MAILBOX_CAPACITY=2048 -e OUTBOUND_CAPACITY=1024 \
  "$image" /app/target/release/study_server >/dev/null
for attempt in {1..50}; do
  if docker exec "$server" bash -c 'echo > /dev/tcp/127.0.0.1/8080' 2>/dev/null; then break; fi
  sleep 0.1
done
docker inspect "$server" --format '{{json .HostConfig}}' > "$results/container.json"
docker version > "$results/docker.txt"
docker exec "$server" cat /sys/fs/cgroup/cpu.stat > "$results/cpu-before.txt"
for mode in echo broadcast; do
  for clients in 64 256 1024; do
    for run in 1 2 3; do
      file="$results/$mode-$clients-$run.json"
      docker run --rm --network "$network" -e TOKIO_WORKER_THREADS=4 "$image" \
        /app/target/release/examples/load "ws://$server:8080/ws" "$clients" "${ROUNDS:-400}" 64 "$mode" 4 8 > "$file"
      cat "$file"
    done
  done
done
docker exec "$server" cat /sys/fs/cgroup/cpu.stat > "$results/cpu-after.txt"
docker exec "$server" cat /sys/fs/cgroup/memory.peak > "$results/memory-peak.txt"
docker exec "$server" cat /sys/fs/cgroup/memory.events > "$results/memory-events.txt"
