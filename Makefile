.PHONY: up down migrate dev test check help web-install web-dev web-build web-test web-docker web-up

help: ## List the available targets
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-10s %s\n", $$1, $$2}'

up: ## Start Postgres, the S3 mock (with the bucket) and Mailpit
	docker compose up -d postgres s3 mailpit

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

web-install: ## Install the web client dependencies
	cd web && bun install

web-dev: ## Run the web client at http://localhost:3002
	cd web && bun run dev

web-build: ## Static export of the web client into web/out
	cd web && bun run build

web-test: ## Run the web client tests (the contract test uses MAIL_API_URL)
	cd web && bun test

web-docker: ## Build the webmail container image (copod-mail-web)
	docker build -t copod-mail-web web

web-up: ## Build and run the webmail container, serving http://localhost:3002
	docker compose up -d --build web
