EGUI_APP_BUNDLE := target/release/bundle/osx/Seam Explorer (egui).app
EGUI_INSTALLED_APP := /Applications/Seam Explorer (egui).app

.PHONY: build run install run-egui test-egui bundle-egui install-egui build-client test-client

build:
	$(MAKE) -C apps/seam-explorer-webview build

run:
	$(MAKE) -C apps/seam-explorer-webview run

install:
	$(MAKE) -C apps/seam-explorer-webview install

# GRAPH=<path> preloads that graph.json at startup instead of requiring the
# Load graph.json dialog on every UI review iteration (plan 05-14). A
# relative path resolves against the directory `make` was invoked from,
# since `cargo run` does not change directory.
run-egui:
	cargo run -p seam-explorer-egui --release$(if $(GRAPH), -- "$(GRAPH)")

test-egui:
	cargo test -p seam-explorer-egui

# The Claude Code PostToolUse hook client. Release, because that is the build
# a user actually registers -- the latency budget in apps/seam-client/README.md
# is stated for this profile. The absolute path is printed so it can be pasted
# straight into the hook configuration block in that README.
build-client:
	cargo build -p seam-client --release
	@echo "Built: $(CURDIR)/target/release/seam-client"

test-client:
	cargo test -p seam-client

bundle-egui:
	cd apps/seam-explorer-egui && cargo bundle --release --format osx

install-egui: bundle-egui
	rm -rf "$(EGUI_INSTALLED_APP)"
	cp -R "$(EGUI_APP_BUNDLE)" /Applications/
	xattr -cr "$(EGUI_INSTALLED_APP)"
	@echo "Installed to $(EGUI_INSTALLED_APP) — launch it from Spotlight or Finder."
