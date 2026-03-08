#!/bin/bash
set -e

# Kill any existing stray processes to prevent port conflicts
echo "Cleaning up any old processes..."
pkill -f 'python3 -m http.server 9000' || true
pkill -f 'main-server' || true
pkill -f 'volunteer-agent' || true

sleep 2

# Navigate to the project directory
cd "$(dirname "$0")"

# Start the mock Docker service in the background
echo "Starting local Python web server on port 9000..."
python3 -m http.server 9000 &
PYTHON_PID=$!

sleep 1

# Export PROTOC so tonic-build can find the compiler
export PROTOC="$(pwd)/protoc_dir/bin/protoc"

# Start the main server in the background
echo "Starting Main Server on ports 50051 (gRPC) and 8080 (HTTP)..."
cargo run --bin main-server > main.log 2>&1 &
SERVER_PID=$!

# Give the server a moment to bind its ports
sleep 3

# Start the volunteer agent in the background
echo "Starting Volunteer Agent (connecting to 50051)..."
cargo run --bin volunteer-agent > vol.log 2>&1 &
AGENT_PID=$!
cargo run --bin volunteer-agent > vol2.log 2>&1 &
AGENT_PID=$!

echo "=========================================================="
echo "All components are running in the background!"
echo ""
echo "Try running:"
echo "  curl -v http://127.0.0.1:8080/Cargo.toml"
echo ""
echo "To view the real-time server logs (including heartbeats), run:"
echo "  tail -f main.log"
echo "To view real-time agent logs, run:"
echo "  tail -f vol.log"
echo "=========================================================="

# Trap SIGINT (Ctrl+C) to clean up all background processes before exiting
trap "echo 'Shutting down...'; kill $PYTHON_PID $SERVER_PID $AGENT_PID; exit 0" SIGINT SIGTERM

# Keep the script alive so the trap can catch exits
wait
