# Builds and packages Seam Explorer (egui) as a Homebrew-Cask-ready .app
# zip, for esumerfd/actions' shared release.yml. This crate is a member of
# the shared root Cargo workspace, so `cargo bundle`'s build output always
# lands under the WORKSPACE ROOT's target/ (../../target/ from here),
# never a local apps/seam-explorer-egui/target/ -- regardless of which
# member directory cargo was invoked from. This Makefile's `release:`
# target existing at all is what makes the shared workflow discover
# seam-explorer-egui as a releasable product (voice-agent's README
# explains the discovery rule: apps/<name>/Makefile with a `release:`
# target).
#
# TARGET is the Rust target triple (e.g. aarch64-apple-darwin,
# x86_64-apple-darwin) -- optional; omitted for a local run, which builds
# for the host's own native target (detected via `rustc -vV`).
#
# Asset naming (seam-explorer-egui-v<version>-<triple>.zip[.sha256])
# matches Casks/seam-explorer-egui.rb's existing url/sha256 interpolation
# exactly, and the .sha256 sidecar's format (a full `shasum -a 256` line,
# not a bare hash) matches what seam-explorer-egui-release.yml already
# produced -- both preserved unchanged by this migration.

ASSET_DIR := dist

.PHONY: release
release:
	@set -e; \
	VERSION=$$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1); \
	if [ -z "$$VERSION" ]; then echo "release: could not read [package] version from Cargo.toml" >&2; exit 1; fi; \
	TRIPLE="$(TARGET)"; \
	if [ -z "$$TRIPLE" ]; then TRIPLE=$$(rustc -vV | sed -n 's/^host: //p'); fi; \
	if ! command -v cargo-bundle >/dev/null 2>&1; then \
		echo "release: installing cargo-bundle 0.11.0"; \
		cargo install cargo-bundle --version 0.11.0 --locked; \
	fi; \
	echo "release: bundling seam-explorer-egui $$VERSION for $$TRIPLE"; \
	cargo bundle --release --format osx --target "$$TRIPLE"; \
	BUNDLE_DIR="../../target/$$TRIPLE/release/bundle/osx"; \
	if [ ! -f "$$BUNDLE_DIR/Seam Explorer (egui).app/Contents/Resources/icon.icns" ]; then \
		echo "release: icon.icns missing from the bundle at $$BUNDLE_DIR -- cargo-bundle likely built from the wrong CWD (its icon glob is CWD-relative, not package-root-relative)" >&2; \
		exit 1; \
	fi; \
	rm -rf $(ASSET_DIR); mkdir -p $(ASSET_DIR); \
	ASSET="seam-explorer-egui-v$${VERSION}-$${TRIPLE}.zip"; \
	( cd "$$BUNDLE_DIR" && ditto -c -k --sequesterRsrc --keepParent "Seam Explorer (egui).app" "$$ASSET" ); \
	mv "$$BUNDLE_DIR/$$ASSET" "$(ASSET_DIR)/$$ASSET"; \
	shasum -a 256 "$(ASSET_DIR)/$$ASSET" > "$(ASSET_DIR)/$$ASSET.sha256"; \
	echo "release: wrote $(ASSET_DIR)/$$ASSET"
