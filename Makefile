VERSION ?= $(shell git describe --tags --always --dirty 2>/dev/null || echo "dev")
BIN := target/release/tmc

.PHONY: build run install clean test lint fmt check

build:
	cargo build --release

run: build
	$(BIN)

install: check build
	rm -f ~/.local/bin/tmc
	cp $(BIN) ~/.local/bin/tmc

test:
	cargo test

lint:
	cargo clippy --all-targets

fmt:
	cargo fmt

# What CI would run: everything that can fail without a terminal.
check: fmt lint test

clean:
	cargo clean
