PREFIX ?= $(HOME)/.local
BINDIR = $(PREFIX)/bin
DATADIR = $(PREFIX)/share
APPDIR = $(DATADIR)/applications
ICONDIR = $(DATADIR)/icons/hicolor/scalable/apps

.PHONY: build install uninstall

build:
	cargo build --release

install: build
	install -Dm755 target/release/fileman $(BINDIR)/fileman
	@if command -v patchelf >/dev/null 2>&1 && [ -n "$$LD_LIBRARY_PATH" ]; then \
		echo "Patching RPATH for NixOS..."; \
		patchelf --set-rpath "$$LD_LIBRARY_PATH" $(BINDIR)/fileman; \
	fi
	install -Dm644 etc/fileman.svg $(ICONDIR)/fileman.svg
	sed 's|Exec=fileman|Exec=$(BINDIR)/fileman|' etc/fileman.desktop \
		| install -Dm644 /dev/stdin $(APPDIR)/fileman.desktop
	@echo "Installed to $(PREFIX). Make sure $(BINDIR) is in your PATH."

uninstall:
	rm -f $(BINDIR)/fileman
	rm -f $(APPDIR)/fileman.desktop
	rm -f $(ICONDIR)/fileman.svg
