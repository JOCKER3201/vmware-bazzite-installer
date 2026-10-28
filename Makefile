# Budowanie, pakowanie (AppImage) i instalacja użytkownika (~/.local).
# Cele: build / package / install / uninstall (+ clean).

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

## build: kompilacja wydania — bez instalowania czegokolwiek
build:
	$(CARGO) build --release

## package: samodzielny AppImage (wymaga appimagetool w PATH)
package: build
	@command -v $(APPIMAGETOOL) >/dev/null 2>&1 || { \
		echo "Brak narzędzia '$(APPIMAGETOOL)'."; \
		echo "Pobierz je z https://github.com/AppImage/appimagetool/releases"; \
		echo "i umieść w PATH (np. ~/.local/bin/appimagetool), albo podaj"; \
		echo "ścieżkę zmienną APPIMAGETOOL=/ścieżka/do/appimagetool."; \
		exit 1; }
	rm -rf $(APPDIR)
	install -D -m 0755 $(TARGET) $(APPDIR)/usr/bin/$(BIN)
	install -D -m 0644 packaging/$(BIN).desktop $(APPDIR)/$(BIN).desktop
	install -D -m 0644 packaging/$(BIN).svg $(APPDIR)/$(BIN).svg
	ln -sf $(BIN).svg $(APPDIR)/.DirIcon
	ln -sf usr/bin/$(BIN) $(APPDIR)/AppRun
	ARCH=$(ARCH) $(APPIMAGETOOL) $(APPDIR) $(APPIMAGE)
	@echo "Gotowe: $(APPIMAGE)"

## install: instalacja dla bieżącego użytkownika (binarka, menu, ikona)
install: build
	install -D -m 0755 $(TARGET) $(DESTDIR)$(PREFIX)/bin/$(BIN)
	install -D -m 0644 packaging/$(BIN).svg \
		$(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps/$(BIN).svg
	install -d -m 0755 $(DESTDIR)$(PREFIX)/share/applications
	sed 's|^Exec=.*|Exec=$(PREFIX)/bin/$(BIN)|' packaging/$(BIN).desktop \
		> $(DESTDIR)$(PREFIX)/share/applications/$(BIN).desktop
	chmod 0644 $(DESTDIR)$(PREFIX)/share/applications/$(BIN).desktop
	-update-desktop-database -q $(DESTDIR)$(PREFIX)/share/applications 2>/dev/null
	@echo "Zainstalowano: $(PREFIX)/bin/$(BIN) (+ wpis w menu aplikacji)"

## uninstall: usunięcie wszystkiego, co zainstalował cel install
uninstall:
	rm -f $(PREFIX)/bin/$(BIN)
	rm -f $(PREFIX)/share/applications/$(BIN).desktop
	rm -f $(PREFIX)/share/icons/hicolor/scalable/apps/$(BIN).svg
	-update-desktop-database -q $(PREFIX)/share/applications 2>/dev/null
	@echo "Odinstalowano."

## clean: usunięcie artefaktów budowania
clean:
	$(CARGO) clean
