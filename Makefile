# Build, packaging (AppImage) and per-user installation (~/.local).
# Targets: build / package / install / uninstall (+ clean).

BIN          := vmware-sysext-builder
PREFIX       ?= $(HOME)/.local
CARGO        ?= cargo
APPIMAGETOOL ?= appimagetool

VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
ARCH    := $(shell uname -m)
TARGET  := target/release/$(BIN)

APPDIR   := target/appimage/AppDir
APPIMAGE := target/appimage/$(BIN)-$(VERSION)-$(ARCH).AppImage

.PHONY: all build package install uninstall clean

all: build

## build: release build — installs nothing
build:
	$(CARGO) build --release

## package: self-contained AppImage (requires appimagetool in PATH)
package: build
	@command -v $(APPIMAGETOOL) >/dev/null 2>&1 || { \
		echo "Tool '$(APPIMAGETOOL)' not found."; \
		echo "Download it from https://github.com/AppImage/appimagetool/releases"; \
		echo "and put it in PATH (e.g. ~/.local/bin/appimagetool), or pass"; \
		echo "its path via APPIMAGETOOL=/path/to/appimagetool."; \
		exit 1; }
	rm -rf $(APPDIR)
	install -D -m 0755 $(TARGET) $(APPDIR)/usr/bin/$(BIN)
	install -D -m 0644 packaging/$(BIN).desktop $(APPDIR)/$(BIN).desktop
	install -D -m 0644 packaging/$(BIN).svg $(APPDIR)/$(BIN).svg
	ln -sf $(BIN).svg $(APPDIR)/.DirIcon
	ln -sf usr/bin/$(BIN) $(APPDIR)/AppRun
	ARCH=$(ARCH) $(APPIMAGETOOL) $(APPDIR) $(APPIMAGE)
	@echo "Done: $(APPIMAGE)"

## install: per-user installation (binary, menu entry, icon)
install: build
	install -D -m 0755 $(TARGET) $(DESTDIR)$(PREFIX)/bin/$(BIN)
	install -D -m 0644 packaging/$(BIN).svg \
		$(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(BIN).svg
	install -d -m 0755 $(DESTDIR)$(PREFIX)/share/applications
	sed 's|^Exec=.*|Exec=$(PREFIX)/bin/$(BIN)|' packaging/$(BIN).desktop \
		> $(DESTDIR)$(PREFIX)/share/applications/$(BIN).desktop
	chmod 0644 $(DESTDIR)$(PREFIX)/share/applications/$(BIN).desktop
	-update-desktop-database -q $(DESTDIR)$(PREFIX)/share/applications 2>/dev/null
	@echo "Installed: $(PREFIX)/bin/$(BIN) (+ application menu entry)"

## uninstall: remove everything the install target installed
uninstall:
	rm -f $(PREFIX)/bin/$(BIN)
	rm -f $(PREFIX)/share/applications/$(BIN).desktop
	rm -f $(PREFIX)/share/icons/hicolor/scalable/apps/$(BIN).svg
	-update-desktop-database -q $(PREFIX)/share/applications 2>/dev/null
	@echo "Uninstalled."

## clean: remove build artifacts
clean:
	$(CARGO) clean
