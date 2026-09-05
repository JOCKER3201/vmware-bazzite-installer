# vmware-sysext-builder

Graficzny kreator, który z oficjalnego pliku instalacyjnego **VMware Workstation
dla Linuksa (`.bundle`)** buduje gotowy obraz **`vmware.raw`** dla
**systemd-sysext** — przeznaczony dla **Bazzite** i innych systemów Fedora
Atomic (obraz systemu tylko do odczytu). Całość napisana w Rust (egui).

> ⚠ **To NIE jest oficjalny instalator VMware.** Projekt nieoficjalny,
> w żaden sposób niezwiązany z Broadcom ani VMware — używasz na własną
> odpowiedzialność.
>
> 🤖 **Stworzony przez sztuczną inteligencję** (Claude, Anthropic) na
> zlecenie i pod nadzorem właściciela repozytorium.

## Co robi

1. **Wskazujesz plik `.bundle`** — pobrany samodzielnie z portalu Broadcom
   (od listopada 2024 Workstation Pro jest darmowy, także komercyjnie;
   program nie pobiera niczego z sieci).
2. **Wykrywa zainstalowane jądra** (`/usr/lib/modules`) i sprawdza obecność
   nagłówków (`kernel-devel`); domyślnie wybiera jądro uruchomione.
   Bazzite ma `gcc`, `make` i dopasowany `kernel-devel` w obrazie bazowym.
3. Po kliknięciu **„Zbuduj vmware.raw”**:
   - rozpakowuje `.bundle` poleceniem `sh <bundle> --extract` (bez
     instalowania czegokolwiek i bez roota),
   - **kompiluje moduły jądra `vmmon` i `vmnet`** dla wybranej wersji jądra
     (`make VM_UNAME=<wersja>`); opcjonalnie z łatanych źródeł lub z łatkami
     AUR (patrz niżej),
   - składa drzewo `/usr` według sprawdzonego układu pakietu AUR
     `vmware-workstation` (binaria, biblioteki, dowiązania do appLoadera,
     bity setuid, pliki .desktop, jednostki systemd),
   - wyłącza w launcherze test modułów (`vmware-modconfig`), który na
     systemie z `/usr` tylko do odczytu prowadzi donikąd,
   - generuje **nadzbiór metadanych `depmod`** (moduły bazowe + vmmon/vmnet),
     dzięki czemu po scaleniu działa zwykłe `modprobe` i nic nie przesłania
     modułów bazowych,
   - **weryfikuje drzewo przed pakowaniem**: struktura (wyłącznie `/usr`),
     obecność `extension-release`, martwe dowiązania oraz zgodność
     `vermagic` modułów z jądrem docelowym (`modinfo`),
   - zapisuje **manifest odtwarzalności** (`/usr/share/vmware-sysext/manifest`
     w obrazie + kopia `vmware-sysext-manifest.txt` obok niego): nazwa
     i SHA-256 pakietu, wersja VMware, jądro, pochodzenie źródeł modułów,
     data builda,
   - buduje obraz **erofs** z etykietami SELinux z systemowego
     `file_contexts` (stockowy Bazzite działa w trybie enforcing).
4. Pokazuje polecenia instalacji albo instaluje od razu przez `pkexec`.

## Instalacja obrazu

Program pokazuje te polecenia po udanym budowaniu (i może je wykonać sam):

```sh
sudo install -D -m 0644 vmware.raw /var/lib/extensions/vmware.raw
sudo restorecon -RF /var/lib/extensions
sudo systemd-sysext refresh --no-reload
sudo systemd-tmpfiles --create /usr/lib/tmpfiles.d/vmware-sysext.conf
sudo systemctl daemon-reload
```

Kolejność jest istotna: `--no-reload` odracza start usług do momentu,
aż `tmpfiles` zasieje `/etc/vmware` — dopiero `daemon-reload` uruchamia
usługi (drop-in `Upholds=`).

To wszystko — obraz zawiera drop-in `Upholds=` dla `multi-user.target`,
więc usługi (ładowanie modułów, sieci vmnet, arbiter USB) startują
automatycznie po scaleniu i po każdym rozruchu. `systemd-sysext.service`
jest na Bazzite domyślnie włączone, więc scalenie przetrwa restart.
Katalog `/etc/vmware` jest zasiewany przez `tmpfiles.d` (typ `C+` — nigdy
nie nadpisuje Twoich zmian). Potem wystarczy uruchomić `vmware`.

Odinstalowanie:

```sh
sudo rm /var/lib/extensions/vmware.raw
sudo systemd-sysext refresh
```

## Budowanie programu

```sh
cargo build --release
./target/release/vmware-sysext-builder
```

## Zgodność wersji i łatki

- **Zalecane pakiety: 25H2 / 26H1** (nowe nazewnictwo roczne Broadcom) —
  ich źródła modułów budują się bez łatek na jądrach 7.x.
- **Pakiety 17.6.x nie kompilują się** na jądrach ≥ 6.15 bez łatek
  społeczności; dla jąder 7.x praktycznie wymagany jest pakiet 25H2/26H1.
- Gdy kompilacja się nie powiedzie, w „Opcjach zaawansowanych” wskaż katalog:
  - ze **źródłami forka** `vmware-host-modules` (układ `vmmon-only/` i
    `vmnet-only/`), np. `philipl/vmware-host-modules` (gałęzie
    `workstation-25h2`, `workstation-26h1`), albo
  - z **łatkami** `vmmon.patch` / `vmnet.patch` (np. checkout pakietu AUR
    `vmware-workstation` — lustro: `github.com/archlinux/aur`, gałąź
    `vmware-workstation`) — zostaną nałożone `patch -p2` na źródła z pakietu.
- Moduły są sprzężone z wersją produktu (odmawiają załadowania przy
  niezgodności) — używaj źródeł/łatek dla tej samej wersji co `.bundle`.

## Ważne uwagi

- **Etykiety SELinux:** gdy budowa obrazu z etykietami się nie powiedzie,
  program na systemie w trybie enforcing przerywa z błędem (obraz bez
  etykiet i tak by nie działał); przy permissive buduje obraz zapasowy bez
  etykiet i wyraźnie to sygnalizuje w panelu wyniku.
- **Aktualizacja jądra** (rpm-ostree / bootc) unieważnia moduły — po każdej
  aktualizacji Bazzite zbuduj i zainstaluj obraz ponownie. Jednostka
  `vmware-modules.service` ma warunek na dokładną wersję jądra, więc po
  aktualizacji po prostu się nie uruchomi (bez błędów przy starcie).
- **Secure Boot:** niepodpisane moduły nie załadują się przy włączonym
  Secure Boot. Klucz Universal Blue (`akmods-ublue`) nie podpisze modułów
  budowanych lokalnie (prywatna część zostaje w CI) — potrzebny własny klucz
  MOK zarejestrowany przez `mokutil --import`. Program potrafi podpisać
  moduły wskazanym kluczem i certyfikatem (opcje zaawansowane).
- Obraz **nie zawiera** komponentów `vmware-installer`, VIX, ovftool ani
  vprobe (zbędne do codziennej pracy; mniejszy obraz).
- Program **nie pobiera ani nie rozprowadza** oprogramowania VMware — obraz
  budowany jest lokalnie z pliku dostarczonego przez użytkownika i zawiera
  oprogramowanie objęte licencją Broadcom/VMware; nie rozpowszechniaj go.
- VMware i VMware Workstation są znakami towarowymi Broadcom Inc. Ten projekt
  nie jest powiązany z Broadcom.

## Licencja

Kod tego narzędzia: [MIT](LICENSE). Układ instalacji odtworzono na podstawie
faktów z pakietu AUR `vmware-workstation` i pakietu NixOS (bez kopiowania
kodu); wzorce sysext według projektów fedora-sysexts i Flatcar.
