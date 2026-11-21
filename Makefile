.PHONY: check test engine addon eval verify-infra clean

SHELL := /usr/bin/env bash
NPROC := $(shell nproc 2>/dev/null || echo 4)

check:
	cargo check --workspace

test:
	cargo test --workspace

engine:
	cargo build --release -p animed

addon:
	cmake -S frontends/fcitx5 -B frontends/fcitx5/build
	cmake --build frontends/fcitx5/build -j$(NPROC)

eval:
	cargo run -p anime-eval -- eval/fixtures/smoke.tsv

verify-infra:
	@echo "==> Checking shell scripts syntax"
	bash -n scripts/*.sh
	@echo "==> Validating YAML registries"
	python3 -c 'import yaml; m = yaml.safe_load(open(".agents/map.yaml")); assert len(m["modules"]) == 11, len(m["modules"]); d = yaml.safe_load(open("data/MANIFEST.yaml")); assert "sources" in d; print("YAML OK:", len(m["modules"]), "modules,", len(d["sources"]), "data sources")'
	@echo "==> Self-testing verify-boundaries.sh against all task cards"
	@./scripts/verify-boundaries.sh .agents/tasks/task-01-data-and-eval.md --check-paths eval/fixtures/smoke.tsv scripts/data_pipeline/build.py data/MANIFEST.yaml crates/eval/src/main.rs
	@./scripts/verify-boundaries.sh .agents/tasks/task-02-lexicon-core.md --check-paths crates/lexicon/src/lib.rs crates/lexicon/src/phonetic.rs
	@./scripts/verify-boundaries.sh .agents/tasks/task-03-ngram-and-engine.md --check-paths crates/protocol/src/lib.rs crates/lm-ngram/src/lib.rs crates/engine/src/lib.rs crates/animed/src/main.rs
	@./scripts/verify-boundaries.sh .agents/tasks/task-04-fcitx5-frontend.md --check-paths frontends/fcitx5/src/anime_engine.cpp frontends/fcitx5/CMakeLists.txt
	@echo "==> Testing negative boundary enforcement (forbidden + out-of-whitelist)"
	@if ./scripts/verify-boundaries.sh .agents/tasks/task-02-lexicon-core.md --check-paths .agents/contracts/core-api.md 2>/dev/null; then \
		echo "ERROR: verify-boundaries.sh failed to reject forbidden contract edit" >&2; exit 1; \
	fi
	@if ./scripts/verify-boundaries.sh .agents/tasks/task-04-fcitx5-frontend.md --check-paths crates/engine/src/lib.rs 2>/dev/null; then \
		echo "ERROR: verify-boundaries.sh failed to reject out-of-whitelist edit" >&2; exit 1; \
	fi
	@echo "INFRA OK"

clean:
	rm -rf target frontends/fcitx5/build
