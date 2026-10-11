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
RUN_FLAGS = --mcp-port $(MCP_PORT) --mcp-host $(MCP_HOST) --window-size $(SIZE) \
  $(if $(MCP_TOKEN),--mcp-token $(MCP_TOKEN)) \
  $(if $(USER_DATA_DIR),--user-data-dir $(USER_DATA_DIR)) $(RUN_ARGS) $(URL)

.DEFAULT_GOAL := help
.PHONY: help deps check-deps build run run-headless test unittests smoke \
        smoke-headed bot-detection regressions fmt lint package clean docker-image docker-build

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

regressions: $(BIN) ## Regression tests (e.g. abandoned DOM requests must not crash a tab).
	python3 $(ROOT)/tests/regressions/abandoned_dom_reply.py --launch $(BIN)

test: unittests smoke regressions bot-detection ## Unit, smoke, regression and bot-detection tests.

# The vendored servoshell keeps upstream's formatting (see
# crates/servoaf-shell/VENDORED.md); only servoaf's own code is formatted.
FMT_FILES = $(ROOT)/crates/servoaf-shell/mcp.rs

fmt: ## Format servoaf's own code.
	$(CARGO) fmt -p servoaf-core
	rustfmt --edition 2024 $(FMT_FILES)

lint: ## rustfmt --check + clippy.
	$(CARGO) fmt -p servoaf-core -- --check
	rustfmt --edition 2024 --check $(FMT_FILES)
	$(CARGO) clippy $(CARGO_PROFILE_FLAG) -p servoaf-core --all-targets -- -D warnings

package: build ## Create dist/servoaf-<version>-<arch>-linux.tar.xz (+ .sha256) for checking in.
	@set -e; version=$$(sed -n 's/^version = "\(.*\)"/\1/p' $(ROOT)/Cargo.toml | head -1); \
	name=servoaf-$$version-$$(uname -m)-linux; stage=$(ROOT)/target/package/$$name; \
	rm -rf $(ROOT)/target/package && mkdir -p $$stage $(ROOT)/dist; \
	cp $(BIN) $(ROOT)/README.md $(ROOT)/LICENSE $$stage/; \
	cp $(ROOT)/scripts/install.sh $$stage/; \
	strip $$stage/servoaf; \
	rm -f $(ROOT)/dist/servoaf-*-linux.tar.xz $(ROOT)/dist/servoaf-*-linux.tar.xz.sha256; \
	tar -C $(ROOT)/target/package --owner=0 --group=0 -cJf $(ROOT)/dist/$$name.tar.xz $$name; \
	(cd $(ROOT)/dist && sha256sum $$name.tar.xz > $$name.tar.xz.sha256); \
	ls -l $(ROOT)/dist/$$name.tar.xz; echo "package: dist/$$name.tar.xz"

clean: ## cargo clean.
	$(CARGO) clean

docker-image: ## Build the Ubuntu 24.04 build image.
	docker build -t $(DOCKER_IMAGE) -f $(ROOT)/Dockerfile $(ROOT)

docker-build: docker-image ## Build inside the image (output lands in target/).
	docker run --rm -v $(ROOT):/src -w /src $(DOCKER_IMAGE) cargo build $(CARGO_PROFILE_FLAG)
