.PHONY: build run install

build:
	$(MAKE) -C seam-explorer build

run:
	$(MAKE) -C seam-explorer run

install:
	$(MAKE) -C seam-explorer install
