REPO_ROOT    := $(shell dirname $(realpath $(firstword $(MAKEFILE_LIST))))
SKILLS_DIR   := $(REPO_ROOT)/skills
PLUGINS_DIR  := $(REPO_ROOT)/plugins
BIN_DIR      := $(REPO_ROOT)/bin
ALLUKA_HOME  := $(HOME)/.alluka

# Auto-detect buildable skills: have go.mod AND at least one main.go
# (excluding integration_test helpers)
BUILDABLE_SKILLS := $(shell \
	for s in $(shell ls $(SKILLS_DIR)); do \
		if [ -f "$(SKILLS_DIR)/$$s/go.mod" ]; then \
			if find "$(SKILLS_DIR)/$$s" -name "main.go" \
				| grep -v "integration_test" | grep -q .; then \
				echo $$s; \
			fi; \
		fi; \
	done)

.PHONY: FORCE help list \
        build build-skills build-plugins build-dashboard \
        install install-skills setup doctor clean uninstall \
        build-all install-all test-all

help: ## Show this help
	@grep -E '^[a-zA-Z_%-]+:.*##' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*##"}; {printf "  %-24s %s\n", $$1, $$2}'

list: ## List all auto-detected buildable skills
	@for s in $(BUILDABLE_SKILLS); do echo "  $$s"; done

$(BIN_DIR):
	mkdir -p $(BIN_DIR)

# ── per-skill pattern rules ──────────────────────────────────────────────────

# build-<skill>: build primary binary → bin/<skill>
build-%: FORCE $(BIN_DIR)
	@skill=$*; \
	d=$(SKILLS_DIR)/$$skill; \
	if [ ! -d "$$d" ]; then echo "Error: skill '$$skill' not found"; exit 1; fi; \
	if   [ -f "$$d/cmd/$$skill/main.go" ];     then pkg="./cmd/$$skill"; \
	elif [ -f "$$d/cmd/$$skill-cli/main.go" ]; then pkg="./cmd/$$skill-cli"; \
	elif [ -f "$$d/main.go" ];                 then pkg="."; \
	else echo "Error: no main package found for '$$skill'"; exit 1; fi; \
	echo "  build  $$skill  ($$pkg)"; \
	cd "$$d" && go build -o $(BIN_DIR)/$$skill $$pkg

# install-<skill>: go install primary package → GOPATH/bin
install-%: FORCE
	@skill=$*; \
	d=$(SKILLS_DIR)/$$skill; \
	if [ ! -d "$$d" ]; then echo "Error: skill '$$skill' not found"; exit 1; fi; \
	if   [ -f "$$d/cmd/$$skill/main.go" ];     then pkg="./cmd/$$skill"; \
	elif [ -f "$$d/cmd/$$skill-cli/main.go" ]; then pkg="./cmd/$$skill-cli"; \
	elif [ -f "$$d/main.go" ];                 then pkg="."; \
	else echo "Error: no main package found for '$$skill'"; exit 1; fi; \
	echo "  install  $$skill  ($$pkg)"; \
	cd "$$d" && go install $$pkg

# test-<skill>: run all tests in the skill module
test-%: FORCE
	@skill=$*; \
	d=$(SKILLS_DIR)/$$skill; \
	if [ ! -d "$$d" ]; then echo "Error: skill '$$skill' not found"; exit 1; fi; \
	echo "  test  $$skill"; \
	cd "$$d" && go test ./...

# ── skills bulk targets (kept for backward compat) ───────────────────────────

build-skills: $(addprefix build-,$(BUILDABLE_SKILLS)) ## Build all skills → bin/
build-all:    $(addprefix build-,$(BUILDABLE_SKILLS)) ## Alias for build-skills
install-all:  $(addprefix install-,$(BUILDABLE_SKILLS)) ## Install all skills to GOPATH/bin
install-skills: $(addprefix install-,$(BUILDABLE_SKILLS)) ## Install all skills to GOPATH/bin
test-all:     $(addprefix test-,$(BUILDABLE_SKILLS)) ## Test all skills

# ── plugins ──────────────────────────────────────────────────────────────────

# Reads the `build` field from each plugins/*/plugin.json and runs it.
# Go plugins use GOWORK=off to isolate from the repo workspace.
# Rust plugins (build starts with "cargo") run cargo directly.
# Plugins without a build field (dashboard, nen) are skipped here.
build-plugins: ## Build all plugins via plugin.json build field
	@for d in $(PLUGINS_DIR)/*/; do \
		name=$$(basename "$$d"); \
		pjson="$$d/plugin.json"; \
		[ -f "$$pjson" ] || continue; \
		build=$$(python3 -c \
			"import json; d=json.load(open('$$pjson')); print(d.get('build',''))" \
			2>/dev/null); \
		[ -n "$$build" ] || { echo "  skip   $$name  (no build field)"; continue; }; \
		echo "  build  $$name"; \
		if echo "$$build" | grep -q "^cargo"; then \
			(cd "$$d" && sh -c "$$build"); \
		else \
			(cd "$$d" && GOWORK=off sh -c "$$build"); \
		fi; \
	done

# build-plugin-<name>: build a single plugin by name
build-plugin-%: FORCE
	@name=$*; \
	d=$(PLUGINS_DIR)/$$name; \
	if [ ! -d "$$d" ]; then echo "Error: plugin '$$name' not found"; exit 1; fi; \
	pjson="$$d/plugin.json"; \
	[ -f "$$pjson" ] || { echo "Error: $$pjson not found"; exit 1; }; \
	build=$$(python3 -c \
		"import json; d=json.load(open('$$pjson')); print(d.get('build',''))" \
		2>/dev/null); \
	[ -n "$$build" ] || { echo "Error: no build field in $$pjson"; exit 1; }; \
	echo "  build  $$name"; \
	if echo "$$build" | grep -q "^cargo"; then \
		(cd "$$d" && sh -c "$$build"); \
	else \
		(cd "$$d" && GOWORK=off sh -c "$$build"); \
	fi

# install-plugin-<name>: install a single plugin by name
install-plugin-%: FORCE
	@name=$*; \
	d=$(PLUGINS_DIR)/$$name; \
	if [ ! -d "$$d" ]; then echo "Error: plugin '$$name' not found"; exit 1; fi; \
	pjson="$$d/plugin.json"; \
	install_cmd=$$(python3 -c \
		"import json; d=json.load(open('$$pjson')); print(d.get('install',''))" \
		2>/dev/null); \
	[ -n "$$install_cmd" ] || { echo "Error: no install field in $$pjson"; exit 1; }; \
	echo "  install  $$name"; \
	(cd "$$d" && sh -c "$$install_cmd")

# dashboard has its own Makefile; wails build produces a universal .app bundle.
build-dashboard: ## Build Nanika.app via wails (darwin/universal)
	@echo "  build  dashboard"
	@cd $(PLUGINS_DIR)/dashboard && wails build -platform darwin/universal

# ── top-level build ──────────────────────────────────────────────────────────

build: build-skills build-plugins build-dashboard ## Build everything (skills + plugins + dashboard)

# ── install ──────────────────────────────────────────────────────────────────

# Reads the `install` field from each plugins/*/plugin.json and runs it from
# within the plugin directory so that $(pwd) expands correctly.
# nen's install.sh builds and installs the scanner binaries to ~/.alluka/.
install: ## Install plugin binaries to ~/bin/ via plugin.json install field
	@mkdir -p $(HOME)/bin
	@for d in $(PLUGINS_DIR)/*/; do \
		name=$$(basename "$$d"); \
		pjson="$$d/plugin.json"; \
		[ -f "$$pjson" ] || continue; \
		install_cmd=$$(python3 -c \
			"import json; d=json.load(open('$$pjson')); print(d.get('install',''))" \
			2>/dev/null); \
		[ -n "$$install_cmd" ] || { echo "  skip   $$name  (no install field)"; continue; }; \
		echo "  install  $$name"; \
		(cd "$$d" && sh -c "$$install_cmd"); \
	done

# ── setup ────────────────────────────────────────────────────────────────────

setup: ## Create ~/.alluka/ dirs, build everything, install binaries
	@echo "--- Creating ~/.alluka/ directories ---"
	@mkdir -p $(ALLUKA_HOME)/bin \
	           $(ALLUKA_HOME)/missions \
	           $(ALLUKA_HOME)/logs \
	           $(ALLUKA_HOME)/workspaces \
	           $(ALLUKA_HOME)/worktrees \
	           $(ALLUKA_HOME)/nen/scanners
	@mkdir -p $(HOME)/bin
	@echo "--- Building ---"
	$(MAKE) build
	@echo "--- Installing ---"
	$(MAKE) install

# ── doctor ───────────────────────────────────────────────────────────────────

# Calls `<binary> doctor --json` for each plugin that declares a binary.
# Continues on error so one unhealthy plugin doesn't block the rest.
doctor: ## Run doctor for each plugin binary
	@for d in $(PLUGINS_DIR)/*/; do \
		name=$$(basename "$$d"); \
		pjson="$$d/plugin.json"; \
		[ -f "$$pjson" ] || continue; \
		binary=$$(python3 -c \
			"import json; d=json.load(open('$$pjson')); print(d.get('binary',''))" \
			2>/dev/null); \
		[ -n "$$binary" ] || { echo "  skip   $$name  (no binary field)"; continue; }; \
		if command -v "$$binary" >/dev/null 2>&1; then \
			echo "  doctor  $$name  ($$binary)"; \
			"$$binary" doctor --json 2>/dev/null || true; \
		else \
			echo "  skip    $$name  ($$binary not in PATH)"; \
		fi; \
	done

# ── clean ────────────────────────────────────────────────────────────────────

uninstall: ## Unload launchd agents and remove plists (macOS only)
	@echo "  unload  com.nanika.orchestrator-daemon"
	@launchctl bootout gui/$$(id -u)/com.nanika.orchestrator-daemon 2>/dev/null || true
	@echo "  unload  com.nanika.nen-daemon"
	@launchctl bootout gui/$$(id -u)/com.nanika.nen-daemon 2>/dev/null || true
	@rm -f $(HOME)/Library/LaunchAgents/com.nanika.orchestrator-daemon.plist
	@rm -f $(HOME)/Library/LaunchAgents/com.nanika.nen-daemon.plist
	@echo "  done    launchd agents removed"

clean: ## Remove bin/, plugin bin/ and Rust target/ build artifacts
	rm -rf $(BIN_DIR)
	@for d in $(PLUGINS_DIR)/*/; do \
		[ -d "$$d/bin" ]    && rm -rf "$$d/bin"    || true; \
		[ -d "$$d/target" ] && rm -rf "$$d/target" || true; \
	done
