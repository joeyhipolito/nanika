BINARY_NAME=obsidian
BUILD_DIR=bin
CMD_DIR=./cmd/obsidian-cli
INSTALL_DIR=$(HOME)/.alluka/bin
LDFLAGS=-ldflags "-s -w"

.PHONY: all build install test test-coverage clean uninstall build-all run fmt vet lint help

all: build

build:
	@mkdir -p $(BUILD_DIR)
	go build $(LDFLAGS) -o $(BUILD_DIR)/$(BINARY_NAME) $(CMD_DIR)

install: build
	@mkdir -p $(INSTALL_DIR)
	@ln -sf $(PWD)/$(BUILD_DIR)/$(BINARY_NAME) $(INSTALL_DIR)/$(BINARY_NAME)
	@echo "Installed $(BINARY_NAME) → $(INSTALL_DIR)/$(BINARY_NAME)"

uninstall:
	@rm -f $(INSTALL_DIR)/$(BINARY_NAME)
	@echo "Uninstalled $(BINARY_NAME)"

test:
	go test ./... -v

test-coverage:
	@mkdir -p $(BUILD_DIR)
	go test ./... -coverprofile=$(BUILD_DIR)/coverage.out
	go tool cover -html=$(BUILD_DIR)/coverage.out -o $(BUILD_DIR)/coverage.html

clean:
	@rm -rf $(BUILD_DIR)

build-all: clean
	GOOS=darwin GOARCH=arm64 go build $(LDFLAGS) -o $(BUILD_DIR)/$(BINARY_NAME)-darwin-arm64 $(CMD_DIR)
	GOOS=darwin GOARCH=amd64 go build $(LDFLAGS) -o $(BUILD_DIR)/$(BINARY_NAME)-darwin-amd64 $(CMD_DIR)
	GOOS=linux GOARCH=amd64 go build $(LDFLAGS) -o $(BUILD_DIR)/$(BINARY_NAME)-linux-amd64 $(CMD_DIR)
	GOOS=linux GOARCH=arm64 go build $(LDFLAGS) -o $(BUILD_DIR)/$(BINARY_NAME)-linux-arm64 $(CMD_DIR)

run: build
	./$(BUILD_DIR)/$(BINARY_NAME)

fmt:
	go fmt ./...

vet:
	go vet ./...

lint: fmt vet

help:
	@echo "Available targets:"
	@echo "  build        Build the binary"
	@echo "  install      Build and symlink to ~/.alluka/bin"
	@echo "  uninstall    Remove symlink from ~/.alluka/bin"
	@echo "  test         Run tests"
	@echo "  test-coverage Run tests with coverage"
	@echo "  clean        Remove build artifacts"
	@echo "  build-all    Cross-compile for all platforms"
	@echo "  fmt          Format code"
	@echo "  vet          Run go vet"
	@echo "  lint         Run fmt + vet"
