.PHONY: build run install run-egui test-egui

build:
	$(MAKE) -C seam-explorer build

run:
	$(MAKE) -C seam-explorer run

install:
	$(MAKE) -C seam-explorer install

run-egui:
	cargo run -p seam-explorer-egui --release

test-egui:
	cargo test -p seam-explorer-egui
