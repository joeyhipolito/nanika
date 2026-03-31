# Nanika Dashboard — build, install, and LaunchAgent management.
#
# Targets:
#   make app             — wails build → build/bin/dashboard.app (universal binary)
#   make install         — copy .app to ~/Applications/Nanika.app
#   make install-agent   — install LaunchAgent (runs make install first)
#   make uninstall-agent — stop and remove the LaunchAgent

BINARY        := dashboard
APP_NAME      := Nanika
INSTALL_DIR   := $(HOME)/Applications
APP_PATH      := $(INSTALL_DIR)/$(APP_NAME).app
BINARY_PATH   := $(APP_PATH)/Contents/MacOS/$(BINARY)

PLIST_LABEL   := com.nanika.dashboard
PLIST_SRC     := com.nanika.dashboard.plist
LAUNCH_AGENTS := $(HOME)/Library/LaunchAgents
PLIST_DEST    := $(LAUNCH_AGENTS)/$(PLIST_LABEL).plist

.PHONY: app install install-agent uninstall-agent

## Build a universal macOS .app bundle via wails build.
app:
	wails build -platform darwin/universal
	@echo "built: build/bin/$(BINARY).app"

## Install the .app to ~/Applications/Nanika.app (builds first if needed).
install: app
	mkdir -p "$(INSTALL_DIR)"
	rm -rf "$(APP_PATH)"
	cp -r "build/bin/$(BINARY).app" "$(APP_PATH)"
	@echo "installed: $(APP_PATH)"

## Install and load a LaunchAgent so the dashboard starts at login.
install-agent: install
	mkdir -p "$(LAUNCH_AGENTS)"
	sed 's|__BINARY_PATH__|$(BINARY_PATH)|g' "$(PLIST_SRC)" > "$(PLIST_DEST)"
	launchctl load "$(PLIST_DEST)"
	@echo "agent loaded: $(PLIST_DEST)"
	@echo "to start now: launchctl start $(PLIST_LABEL)"

## Unload and delete the LaunchAgent.
uninstall-agent:
	-launchctl unload "$(PLIST_DEST)" 2>/dev/null
	-rm -f "$(PLIST_DEST)"
	@echo "agent removed"
