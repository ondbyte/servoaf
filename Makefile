# servoaf — Servo with a built-in MCP control server.
#
# Quick start (Linux):
#   make deps          # system packages (sudo) + Rust toolchain
#   make build         # cargo build --release
#   make run           # window, MCP on http://127.0.0.1:$(MCP_PORT)/mcp
#   make run-headless  # no display needed
#   make test          # unit tests + end-to-end smoke test (headless)
#
# It's a plain Cargo workspace: `cargo build --release` and
# `target/release/servoaf --help` work as usual.

SHELL := /bin/bash
ROOT := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))

CARGO ?= cargo
PROFILE ?= release
JOBS ?= $(shell nproc)
BIN := $(ROOT)/target/$(PROFILE)/servoaf

MCP_PORT ?= 9223
MCP_HOST ?= 127.0.0.1
MCP_TOKEN ?=
SIZE ?= 1280x800
URL ?=
USER_DATA_DIR ?=
RUN_ARGS ?=

DOCKER_IMAGE ?= servoaf-build

CARGO_PROFILE_FLAG := $(if $(filter release,$(PROFILE)),--release,--profile $(PROFILE))
RUN_FLAGS = --mcp-port $(MCP_PORT) --mcp-host $(MCP_HOST) --size $(SIZE) \
  $(if $(MCP_TOKEN),--mcp-token $(MCP_TOKEN)) \
  $(if $(USER_DATA_DIR),--user-data-dir $(USER_DATA_DIR)) $(RUN_ARGS) $(URL)

.DEFAULT_GOAL := help
.PHONY: help deps check-deps build run run-headless test unittests smoke \
        smoke-headed bot-detection fmt lint package clean docker-image docker-build

help: ## Show this help.
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z_-]+:.*## / \
	  {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)
	@echo
	@echo "  Variables: MCP_PORT=$(MCP_PORT) MCP_HOST=$(MCP_HOST) SIZE=$(SIZE) PROFILE=$(PROFILE)"
	@echo "             MCP_TOKEN=... URL=... USER_DATA_DIR=... RUN_ARGS=..."

deps: ## Install system packages (uses sudo) and the Rust toolchain.
	@$(ROOT)/scripts/install-deps.sh

check-deps: ## Only report missing build tools.
	@$(ROOT)/scripts/install-deps.sh --check

build: ## Build target/$(PROFILE)/servoaf.
	$(CARGO) build $(CARGO_PROFILE_FLAG) -j $(JOBS)

$(BIN):
	$(MAKE) build

run: $(BIN) ## Open a window with the MCP server enabled.
	$(BIN) $(RUN_FLAGS)

run-headless: $(BIN) ## Run offscreen (servers, CI, no display).
	$(BIN) --headless $(RUN_FLAGS)

unittests: ## cargo test.
	$(CARGO) test $(CARGO_PROFILE_FLAG) -j $(JOBS) -p servoaf-core

smoke: $(BIN) ## Launch a fresh headless instance and exercise every MCP tool.
	python3 $(ROOT)/scripts/mcp_smoke_test.py --launch $(BIN) \
	  --url http://127.0.0.1:$(MCP_PORT)/mcp $(if $(MCP_TOKEN),--token $(MCP_TOKEN))

smoke-headed: $(BIN) ## Same, in a window (needs a display, or xvfb-run make smoke-headed).
	python3 $(ROOT)/scripts/mcp_smoke_test.py --launch $(BIN) --headed \
	  --url http://127.0.0.1:$(MCP_PORT)/mcp $(if $(MCP_TOKEN),--token $(MCP_TOKEN))

bot-detection: $(BIN) ## Fail if industry-standard detectors identify servoaf as a bot (no CAPTCHAs).
	python3 $(ROOT)/tests/bot-detection/run.py --launch $(BIN) \
	  --url http://127.0.0.1:$(MCP_PORT)/mcp $(if $(MCP_TOKEN),--token $(MCP_TOKEN))

test: unittests smoke bot-detection ## Unit tests + smoke test + bot-detection test.

fmt: ## Format the code.
	$(CARGO) fmt --all

lint: ## rustfmt --check + clippy.
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy $(CARGO_PROFILE_FLAG) --all-targets -- -D warnings

package: build ## Create target/servoaf-<version>-<arch>-linux.tar.xz.
	@set -e; version=$$(sed -n 's/^version = "\(.*\)"/\1/p' $(ROOT)/Cargo.toml | head -1); \
	name=servoaf-$$version-$$(uname -m)-linux; dir=$(ROOT)/target/$$name; \
	rm -rf $$dir && mkdir -p $$dir; \
	cp $(BIN) $(ROOT)/README.md $(ROOT)/LICENSE $$dir/; \
	strip $$dir/servoaf || true; \
	tar -C $(ROOT)/target -cJf $(ROOT)/target/$$name.tar.xz $$name; \
	echo "package: target/$$name.tar.xz"

clean: ## cargo clean.
	$(CARGO) clean

docker-image: ## Build the Ubuntu 24.04 build image.
	docker build -t $(DOCKER_IMAGE) -f $(ROOT)/Dockerfile $(ROOT)

docker-build: docker-image ## Build inside the image (output lands in target/).
	docker run --rm -v $(ROOT):/src -w /src $(DOCKER_IMAGE) cargo build $(CARGO_PROFILE_FLAG)
