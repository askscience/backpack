.PHONY: build run test clean docker-build docker-up docker-down help

build:
	cargo build --release

run:
	cargo run --release

dev:
	RUST_LOG=debug cargo run

test:
	cargo test

clean:
	cargo clean
	rm -rf uploads/* data/*

lint:
	cargo clippy -- -D warnings

fmt:
	cargo fmt -- --check

docker-build:
	docker compose build

docker-up:
	docker compose up -d

docker-down:
	docker compose down

docker-logs:
	docker compose logs -f

help:
	@echo "AI Cloud Backpack — Makefile"
	@echo ""
	@echo "Usage:"
	@echo "  make build          Build release binary"
	@echo "  make dev            Run in development mode (debug logging)"
	@echo "  make run            Run release binary"
	@echo "  make test           Run tests"
	@echo "  make lint           Run clippy lints"
	@echo "  make fmt            Check formatting"
	@echo "  make clean          Clean build artifacts and data"
	@echo "  make docker-build   Build Docker image"
	@echo "  make docker-up      Start with docker-compose"
	@echo "  make docker-down    Stop docker-compose"
	@echo "  make docker-logs    Tail docker logs"
