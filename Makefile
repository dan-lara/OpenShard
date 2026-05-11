# OpenShard — Local Development Makefile
# Usage: make <target>

REGISTRAR_BIN     := target/release/registrar
VOLUNTEER_IMAGE   := openshard/volunteer
VOLUNTEER_PORT    := 8080
REGISTRAR_PORT    := 3000

# ── Colors ────────────────────────────────────────────────────────────────────
BOLD  := \033[1m
RESET := \033[0m
GREEN := \033[32m
CYAN  := \033[36m

.PHONY: help build build-release run-server run-vol vol-build vol-run test clean fmt lint

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
		-e REGISTRAR_URL=http://localhost:$(REGISTRAR_PORT) \
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

# ── Deploy ────────────────────────────────────────────────────────────────────
build-deploy:
	@echo "$(BOLD)Building release binary...$(RESET)"
	bash scripts/build.sh

deploy: build-deploy
	@echo "$(BOLD)Deploying to Proxmox...$(RESET)"
	bash scripts/deploy.sh

deploy-only:
	@echo "$(BOLD)Deploying without rebuild...$(RESET)"
	bash scripts/deploy.sh