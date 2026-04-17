BINARY_NAME=tracker
INSTALL_DIR=$(HOME)/.alluka/bin

.PHONY: all build install uninstall clean help

all: build

build:
	cargo build --release

install: build
	@mkdir -p $(INSTALL_DIR)
	@cp target/release/$(BINARY_NAME) $(INSTALL_DIR)/$(BINARY_NAME)
	@echo "Installed $(BINARY_NAME) → $(INSTALL_DIR)/$(BINARY_NAME)"

uninstall:
	@rm -f $(INSTALL_DIR)/$(BINARY_NAME)
	@echo "Uninstalled $(BINARY_NAME)"

clean:
	cargo clean

help:
	@echo "Available targets:"
	@echo "  build        Build the binary (cargo build --release)"
	@echo "  install      Build and copy to ~/.alluka/bin"
	@echo "  uninstall    Remove binary from ~/.alluka/bin"
	@echo "  clean        Remove build artifacts (cargo clean)"
