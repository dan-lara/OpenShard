# OpenShard — Local Development Makefile
# Usage: make <target>

VOLUNTEER_IMAGE   := openshard/volunteer
VOLUNTEER_PORT    := 8080
REGISTRAR_PORT    := 3000
CORE_SERVER_DIR   := src/core-server
REGISTRY          := 10.10.10.10:5000
SVC_DIR           := src/shard-node/container/test-service
CONTROLLER_API    := http://10.10.10.10:$(REGISTRAR_PORT)

# ── Colors ────────────────────────────────────────────────────────────────────
BOLD  := \033[1m
RESET := \033[0m
GREEN := \033[32m
CYAN  := \033[36m

.PHONY: help build build-release run-server vol-build vol-run vol-stop vol-logs \
        svc-build svc-push svc-register test clean fmt lint deploy

# ── Default ───────────────────────────────────────────────────────────────────
help:
	@echo ""
	@echo "$(BOLD)OpenShard — Available targets$(RESET)"
	@echo ""
	@echo "  $(CYAN)build$(RESET)          Build workspace (debug)"
	@echo "  $(CYAN)build-release$(RESET)  Build registrar (release)"
	@echo "  $(CYAN)run-server$(RESET)     Run registrar locally on :$(REGISTRAR_PORT)"
	@echo "  $(CYAN)vol-build$(RESET)      Build volunteer Docker image"
	@echo "  $(CYAN)vol-run$(RESET)        Run volunteer container on :$(VOLUNTEER_PORT)"
	@echo "  $(CYAN)vol-stop$(RESET)       Stop volunteer container"
	@echo "  $(CYAN)svc-build$(RESET)      Build the 3 test service images"
	@echo "  $(CYAN)svc-push$(RESET)       Build + push test services to internal registry"
	@echo "  $(CYAN)svc-register$(RESET)   Register test services with the controller API"
	@echo "  $(CYAN)deploy$(RESET)         Build core-server Docker images"
	@echo "  $(CYAN)test$(RESET)           Run all tests"
	@echo "  $(CYAN)fmt$(RESET)            Format all code"
	@echo "  $(CYAN)lint$(RESET)           Run clippy"
	@echo "  $(CYAN)clean$(RESET)          Remove build artifacts"
	@echo ""

# ── Build ─────────────────────────────────────────────────────────────────────
build:
	@echo "$(BOLD)Building workspace (debug)...$(RESET)"
	cargo build --workspace

build-release:
	@echo "$(BOLD)Building registrar (release)...$(RESET)"
	cargo build --release -p registrar

# ── Run ───────────────────────────────────────────────────────────────────────
run-server:
	@echo "$(BOLD)Starting registrar on :$(REGISTRAR_PORT)...$(RESET)"
	RUST_LOG=debug cargo run -p registrar

# ── Volunteer Docker ──────────────────────────────────────────────────────────
vol-build:
	@echo "$(BOLD)Building volunteer image...$(RESET)"
	docker build -t $(VOLUNTEER_IMAGE) ./src/shard-node/container

vol-run:
	@echo "$(BOLD)Starting volunteer container on :$(VOLUNTEER_PORT)...$(RESET)"
	docker run --rm -d \
		--name openshard-volunteer \
		--network host \
		-v /var/run/docker.sock:/var/run/docker.sock \
		-e REGISTRAR_URL=http://openshard.danlara.com.br:$(REGISTRAR_PORT) \
		-e TUNNEL_SERVER=openshard.danlara.com.br \
		-e SERVICE_PORT=$(VOLUNTEER_PORT) \
		-e HOSTNAME=volunteer-local \
		$(VOLUNTEER_IMAGE)
	@echo "$(GREEN)Volunteer running. Logs: make vol-logs$(RESET)"

vol-stop:
	@echo "$(BOLD)Stopping volunteer container...$(RESET)"
	docker stop openshard-volunteer 2>/dev/null || true

vol-logs:
	docker logs -f openshard-volunteer

# ── Quality ───────────────────────────────────────────────────────────────────
test:
	@echo "$(BOLD)Running tests...$(RESET)"
	cargo test --workspace

fmt:
	@echo "$(BOLD)Formatting code...$(RESET)"
	cargo fmt --all

lint:
	@echo "$(BOLD)Running clippy...$(RESET)"
	cargo clippy --workspace -- -D warnings

# ── Clean ─────────────────────────────────────────────────────────────────────
clean:
	@echo "$(BOLD)Cleaning build artifacts...$(RESET)"
	cargo clean

# ── Test services ─────────────────────────────────────────────────────────────
svc-build:
	@echo "$(BOLD)Building test service images...$(RESET)"
	docker build --build-arg PAGE=index.html     -t $(REGISTRY)/svc-index:latest $(SVC_DIR)
	docker build --build-arg PAGE=service-a.html -t $(REGISTRY)/svc-a:latest     $(SVC_DIR)
	docker build --build-arg PAGE=service-b.html -t $(REGISTRY)/svc-b:latest     $(SVC_DIR)

svc-push: svc-build
	@echo "$(BOLD)Pushing test services to registry $(REGISTRY)...$(RESET)"
	docker push $(REGISTRY)/svc-index:latest
	docker push $(REGISTRY)/svc-a:latest
	docker push $(REGISTRY)/svc-b:latest
	@echo "$(GREEN)Done. Run 'make svc-register' to register them with the controller.$(RESET)"

svc-register:
	@echo "$(BOLD)Registering test services with controller...$(RESET)"
	curl -s -X POST $(CONTROLLER_API)/services \
		-H 'Content-Type: application/json' \
		-d '{"name":"svc-index","domain":"openshard.danlara.com.br","image":"$(REGISTRY)/svc-index:latest","port":80}' \
		| python3 -m json.tool
	curl -s -X POST $(CONTROLLER_API)/services \
		-H 'Content-Type: application/json' \
		-d '{"name":"svc-a","domain":"svc-a.openshard.danlara.com.br","image":"$(REGISTRY)/svc-a:latest","port":80}' \
		| python3 -m json.tool
	curl -s -X POST $(CONTROLLER_API)/services \
		-H 'Content-Type: application/json' \
		-d '{"name":"svc-b","domain":"svc-b.openshard.danlara.com.br","image":"$(REGISTRY)/svc-b:latest","port":80}' \
		| python3 -m json.tool

# ── Deploy ────────────────────────────────────────────────────────────────────
deploy:
	@echo "$(BOLD)Building core-server Docker images...$(RESET)"
	docker compose -f $(CORE_SERVER_DIR)/docker-compose.yml build
	@echo "$(GREEN)Images built. Transfer and run with: docker compose up -d$(RESET)"