INSTALL_DIR=$(HOME)/.alluka/bin
SCANNERS_DIR=$(HOME)/.alluka/nen/scanners

.PHONY: all install uninstall help

all: install

install:
	@bash install.sh

uninstall:
	@rm -f $(INSTALL_DIR)/shu $(INSTALL_DIR)/ko $(INSTALL_DIR)/nen-daemon
	@rm -f $(SCANNERS_DIR)/gyo $(SCANNERS_DIR)/en $(SCANNERS_DIR)/ryu
	@echo "Uninstalled nen components"

help:
	@echo "Available targets:"
	@echo "  install      Build and install all nen components to ~/.alluka/bin"
	@echo "               shu, ko, nen-daemon → ~/.alluka/bin/"
	@echo "               gyo, en, ryu        → ~/.alluka/nen/scanners/"
	@echo "  uninstall    Remove all installed nen components"
