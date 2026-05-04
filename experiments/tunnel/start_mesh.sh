#!/bin/bash
set -e

# Navigate to the experiment directory so relative cert paths resolve correctly
cd "$(dirname "$0")"

# Kill any stray processes from a previous run
echo "Cleaning up any old processes..."
pkill -f 'python3 -m http.server 9000' || true
pkill -f 'main-server' || true
pkill -f 'volunteer-agent' || true

sleep 1

# Start the mock target service (simulates the volunteer's locally hosted app)
echo "Starting local Python web server on port 9000..."
python3 -m http.server 9000 &
PYTHON_PID=$!

sleep 1

# Start the main server (gRPC :50051, HTTP :8080)
echo "Starting main-server on ports 50051 (gRPC/TLS) and 8080 (HTTP)..."
cargo run --bin main-server > main.log 2>&1 &
SERVER_PID=$!

# Give the server time to bind its ports
sleep 3

# Start two volunteer agents
echo "Starting volunteer agents (connecting to localhost:50051)..."
cargo run --bin volunteer-agent > vol1.log 2>&1 &
VOL1_PID=$!
cargo run --bin volunteer-agent > vol2.log 2>&1 &
VOL2_PID=$!

echo "=========================================================="
echo "All components running. Try:"
echo ""
echo "  curl http://127.0.0.1:8080/"
echo "  open http://127.0.0.1:8080/dashboard"
echo ""
echo "Follow logs:"
echo "  tail -f main.log"
echo "  tail -f vol1.log"
echo "=========================================================="

trap "echo 'Shutting down...'; kill $PYTHON_PID $SERVER_PID $VOL1_PID $VOL2_PID 2>/dev/null; exit 0" SIGINT SIGTERM

wait
