.PHONY: check test engine engine-llama engine-vulkan addon eval eval-word eval-sentence eval-modes verify-infra clean

SHELL := /usr/bin/env bash
NPROC := $(shell nproc 2>/dev/null || echo 4)

check:
	cargo check --workspace

test:
	cargo test --workspace

engine:
	cargo build --release -p animed

engine-llama:
	cargo build --release -p animed --features llama

engine-vulkan:
	cargo build --release -p animed --features vulkan

addon:
	cmake -S frontends/fcitx5 -B frontends/fcitx5/build
	cmake --build frontends/fcitx5/build -j$(NPROC)

eval:
	cargo run -p anime-eval -- eval/fixtures/smoke.tsv

# 分模式评测（L1+L2，无 L3）：单词模式（space_commits=true）/ 整句模式（space_commits=false）各跑一遍冻结集，
# 输出报告与逐行结果（<set>.<mode>.txt / <set>.<mode>.rows.tsv）。口径见 crates/eval/README.md。
# 覆盖 profile：make eval-word EVAL_PROFILE="--profile word.space_edits.penalty=0.9"
EVAL_LEX ?= data/compiled/lexicon.animelex.bin
EVAL_NGRAM ?= $(HOME)/.local/share/anime/ngram.animengr.bin
EVAL_SETS ?= core_v2 noise_v2 ood_dev_v1 ood_academic_v1 ood_tm2_v1
EVAL_OUT ?= target/eval-modes
EVAL_PROFILE ?=
EVAL_BIN = $${CARGO_TARGET_DIR:-target}/release/anime-eval

define run_mode_sets
	cargo build --release -p anime-eval
	mkdir -p $(EVAL_OUT)
	for set in $(EVAL_SETS); do \
		echo "==> $$set ($(1) mode)"; \
		$(EVAL_BIN) eval/benchmarks/$$set.tsv --lexicon $(EVAL_LEX) --ngram $(EVAL_NGRAM) \
			--group-by-tag --mode $(1) $(EVAL_PROFILE) \
			--dump-rows $(EVAL_OUT)/$$set.$(1).rows.tsv > $(EVAL_OUT)/$$set.$(1).txt || exit 1; \
		grep -E '^TOTAL' $(EVAL_OUT)/$$set.$(1).txt; \
	done
endef

eval-word:
	$(call run_mode_sets,word)

eval-sentence:
	$(call run_mode_sets,sentence)

eval-modes: eval-word eval-sentence

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
