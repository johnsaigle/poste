PREFIX ?= ~/.local
BIN_DIR := $(PREFIX)/bin
CARGO := cargo

.PHONY: build release install test clippy fmt index search stats doctor clean

build:
	$(CARGO) build

release:
	$(CARGO) build --release

install: release
	$(CARGO) install --path .
	@echo "poste installed to $(shell $(CARGO) --version >/dev/null 2>&1 && echo '$(BIN_DIR)')"

test:
	$(CARGO) test

clippy:
	$(CARGO) clippy --all-targets --all-features

fmt:
	$(CARGO) fmt

# Ingest the latest export into the configured archive root (see
# ~/.config/poste/config.toml). Pass EXPORT=/path/to/mail_<ts> to override.
index: release
ifdef EXPORT
	./target/release/poste ingest "$(EXPORT)"
else
	./target/release/poste ingest --latest
endif

search: release
	./target/release/poste search "$(QUERY)" $(FLAGS)

stats: release
	./target/release/poste stats

doctor: release
	./target/release/poste doctor

clean:
	$(CARGO) clean
