TAURI := $(CURDIR)/node_modules/.bin/tauri
APP_BUNDLE := ../target/release/bundle/macos/Seam Explorer.app
INSTALLED_APP := /Applications/Seam Explorer.app

.PHONY: build run install setup

setup: node_modules/.bin/tauri

node_modules/.bin/tauri:
	npm install

build: setup
	$(TAURI) build --bundles app

run: setup
	$(TAURI) dev

install: build
	rm -rf "$(INSTALLED_APP)"
	cp -R "$(APP_BUNDLE)" /Applications/
	xattr -cr "$(INSTALLED_APP)"
	@echo "Installed to $(INSTALLED_APP) — launch it from Spotlight or Finder."
