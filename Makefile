EGUI_APP_BUNDLE := target/release/bundle/osx/Seam Explorer (egui).app
EGUI_INSTALLED_APP := /Applications/Seam Explorer (egui).app

.PHONY: build run install run-egui test-egui bundle-egui install-egui

build:
	$(MAKE) -C seam-explorer build

run:
	$(MAKE) -C seam-explorer run

install:
	$(MAKE) -C seam-explorer install

# GRAPH=<path> preloads that graph.json at startup instead of requiring the
# Load graph.json dialog on every UI review iteration (plan 05-14). A
# relative path resolves against the directory `make` was invoked from,
# since `cargo run` does not change directory.
run-egui:
	cargo run -p seam-explorer-egui --release$(if $(GRAPH), -- "$(GRAPH)")

test-egui:
	cargo test -p seam-explorer-egui

bundle-egui:
	cd seam-explorer-egui && cargo bundle --release --format osx

install-egui: bundle-egui
	rm -rf "$(EGUI_INSTALLED_APP)"
	cp -R "$(EGUI_APP_BUNDLE)" /Applications/
	xattr -cr "$(EGUI_INSTALLED_APP)"
	@echo "Installed to $(EGUI_INSTALLED_APP) — launch it from Spotlight or Finder."
