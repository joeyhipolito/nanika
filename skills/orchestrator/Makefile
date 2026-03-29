.PHONY: build install test clean gen-eval-prompt gen-eval-persona-selector

BINARY_NAME=orchestrator

build:
	go build -o bin/$(BINARY_NAME) .

install: build
	mkdir -p $(HOME)/bin
	ln -sf $(CURDIR)/bin/$(BINARY_NAME) $(HOME)/bin/$(BINARY_NAME)

test:
	go test ./...

clean:
	rm -rf bin/

# Regenerate the promptfoo eval prompt from the live orchestrator prompt builder.
# Usage:
#   make gen-eval-prompt OUT=/abs/path/to/decomposer.txt
gen-eval-prompt:
	@test -n "$(OUT)" || (echo "OUT=/abs/path/to/decomposer.txt is required" >&2; exit 1)
	go run ./cmd/gen-decomposer-prompt > "$(OUT)"

# Regenerate the promptfoo persona-selector prompt from the live persona catalog.
# Usage:
#   make gen-eval-persona-selector OUT=/abs/path/to/persona-selector.txt
gen-eval-persona-selector:
	@test -n "$(OUT)" || (echo "OUT=/abs/path/to/persona-selector.txt is required" >&2; exit 1)
	go run ./cmd/gen-persona-selector-prompt > "$(OUT)"
