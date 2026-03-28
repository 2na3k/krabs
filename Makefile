COMPOSE      = docker compose -f docker/docker-compose.krabs.yml --env-file .env
COMPOSE_FULL = docker compose -f docker-compose.yml -f docker/docker-compose.krabs.yml --env-file .env

# ── Build ─────────────────────────────────────────────────────────────────────

.PHONY: build
build:
	$(COMPOSE) build

.PHONY: build-server
build-server:
	$(COMPOSE) build krabs-server

.PHONY: build-gateway
build-gateway:
	$(COMPOSE) build krabs-gateway

# ── Up / Down ─────────────────────────────────────────────────────────────────

.PHONY: up
up:
	$(COMPOSE) up -d

.PHONY: up-full
up-full:
	$(COMPOSE_FULL) up -d

.PHONY: down
down:
	$(COMPOSE) down

.PHONY: down-full
down-full:
	$(COMPOSE_FULL) down

# ── Logs ──────────────────────────────────────────────────────────────────────

.PHONY: logs
logs:
	$(COMPOSE) logs -f

.PHONY: logs-server
logs-server:
	$(COMPOSE) logs -f krabs-server

.PHONY: logs-gateway
logs-gateway:
	$(COMPOSE) logs -f krabs-gateway

# ── Lifecycle ─────────────────────────────────────────────────────────────────

# restart = recreate so updated .env vars are picked up
.PHONY: restart
restart:
	$(COMPOSE) up -d

.PHONY: restart-server
restart-server:
	$(COMPOSE) up -d krabs-server

.PHONY: restart-gateway
restart-gateway:
	$(COMPOSE) up -d krabs-gateway

.PHONY: ps
ps:
	$(COMPOSE) ps

# ── Dev helpers ───────────────────────────────────────────────────────────────

.PHONY: rebuild
rebuild:
	$(COMPOSE) up -d --build

.PHONY: clean
clean:
	$(COMPOSE) down -v --rmi local

.PHONY: shell-server
shell-server:
	$(COMPOSE) exec krabs-server sh

.PHONY: shell-gateway
shell-gateway:
	$(COMPOSE) exec krabs-gateway sh

# ── Cargo (local) ─────────────────────────────────────────────────────────────

.PHONY: fmt
fmt:
	cargo fmt

.PHONY: check
check:
	cargo build && cargo clippy --all-targets -- -D warnings

.PHONY: test
test:
	cargo test --workspace
