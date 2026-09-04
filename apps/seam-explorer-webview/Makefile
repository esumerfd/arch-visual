TAURI := $(CURDIR)/node_modules/.bin/tauri
APP_BUNDLE := ../../target/release/bundle/macos/Seam Explorer (webview).app
INSTALLED_APP := /Applications/Seam Explorer (webview).app

ASSET_DIR := dist

.PHONY: build run install setup release

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

# Builds and packages Seam Explorer (webview) for esumerfd/actions' shared
# release.yml. Version comes from tauri.conf.json's own `version` field --
# the field confirmed to actually drive the shipped
# CFBundleShortVersionString, NOT this crate's Cargo.toml (which is only a
# fallback when tauri.conf.json omits its own version -- it doesn't, here).
#
# TARGET is the Rust target triple -- optional; omitted for a local run,
# which builds for the host's own native target. tauri.conf.json's
# `bundle.targets: ["app", "dmg"]` means a plain `tauri build --target
# <triple>` (no --bundles override, unlike the `build:` target above which
# only wants the .app for local `make install`) produces both.
#
# FIXES an inherited defect from the old seam-explorer-webview-release.yml
# (deliberately not fixed there per its own comment, since that workflow
# is retired by this migration): it uploaded the .app bundle DIRECTORY
# directly via a glob -- GitHub Release assets must be files, and
# esumerfd/actions' shared workflow would hit the same problem trying to
# upload dist/* if a directory landed there. The .app is zipped here
# (mirroring seam-explorer-egui's own Makefile), same as the .dmg is
# already a proper file needing no such treatment.
release:
	@set -e; \
	VERSION=$$(jq -r '.version' tauri.conf.json); \
	if [ -z "$$VERSION" ] || [ "$$VERSION" = "null" ]; then echo "release: could not read .version from tauri.conf.json" >&2; exit 1; fi; \
	TRIPLE="$(TARGET)"; \
	if [ -z "$$TRIPLE" ]; then TRIPLE=$$(rustc -vV | sed -n 's/^host: //p'); fi; \
	$(MAKE) setup; \
	echo "release: building seam-explorer-webview $$VERSION for $$TRIPLE"; \
	$(TAURI) build --target "$$TRIPLE"; \
	BUNDLE_DIR="../../target/$$TRIPLE/release/bundle"; \
	APP="$$BUNDLE_DIR/macos/Seam Explorer (webview).app"; \
	DMG_SRC=$$(find "$$BUNDLE_DIR/dmg" -maxdepth 1 -name '*.dmg' | head -1); \
	if [ ! -d "$$APP" ]; then echo "release: $$APP not found after tauri build" >&2; exit 1; fi; \
	if [ -z "$$DMG_SRC" ]; then echo "release: no .dmg found under $$BUNDLE_DIR/dmg after tauri build" >&2; exit 1; fi; \
	rm -rf $(ASSET_DIR); mkdir -p $(ASSET_DIR); \
	APP_ZIP="seam-explorer-webview-v$${VERSION}-$${TRIPLE}.zip"; \
	( cd "$$BUNDLE_DIR/macos" && ditto -c -k --sequesterRsrc --keepParent "Seam Explorer (webview).app" "$$APP_ZIP" ); \
	mv "$$BUNDLE_DIR/macos/$$APP_ZIP" "$(ASSET_DIR)/$$APP_ZIP"; \
	DMG_ASSET="seam-explorer-webview-v$${VERSION}-$${TRIPLE}.dmg"; \
	cp "$$DMG_SRC" "$(ASSET_DIR)/$$DMG_ASSET"; \
	shasum -a 256 "$(ASSET_DIR)/$$APP_ZIP" > "$(ASSET_DIR)/$$APP_ZIP.sha256"; \
	shasum -a 256 "$(ASSET_DIR)/$$DMG_ASSET" > "$(ASSET_DIR)/$$DMG_ASSET.sha256"; \
	echo "release: wrote $(ASSET_DIR)/$$APP_ZIP and $(ASSET_DIR)/$$DMG_ASSET"
