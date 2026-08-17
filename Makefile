.PHONY: build release run format lint test model

build:
	cargo build

release:
	cargo build --release

run:
	cargo run

format:
	cargo fmt --all

lint:
	cargo clippy --all-targets --all-features -- -D warnings

test:
	cargo test --all-targets

model:
	./scripts/download-model.sh
