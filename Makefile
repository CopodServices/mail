.PHONY: up down migrate dev test check help

help: ## List the available targets
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-10s %s\n", $$1, $$2}'

up: ## Start Postgres, MinIO (with the bucket) and Mailpit
	docker compose up -d postgres minio minio-init mailpit

down: ## Stop the local stack
	docker compose down

migrate: ## Apply database migrations
	cargo run -p migrator

dev: ## Run the mail service against the local stack
	cargo run -p server

test: ## Run the unit tests
	cargo test --all

check: ## Format check and clippy, matching CI
	cargo fmt --all --check
	cargo clippy --all-targets --all-features -- -D warnings
