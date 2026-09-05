# Jak budować obrazy systemd-sysext (`.raw`) — metodologia i pułapki

Ten dokument to destylat z realnej pracy nad `dms-fedora-sysext` (Hyprland +
DankMaterialShell na Bazzite/Kinoite). Adresowany do AI budującego podobne
obrazy sysext — każda zasada tu ma za sobą konkretny incydent z tamtego
projektu, nie jest teorią.

## 1. Fundamentalna zasada: sprawdź bazę, zanim cokolwiek dodasz

Sysext ma sens tylko wtedy, gdy NIE dubluje tego, co host już ma. Przed
spakowaniem jakiejkolwiek biblioteki/binarki do obrazu:

1. Sprawdź, czy plik o tej samej nazwie (SONAME) już istnieje na hoście:
   `find /usr/lib64 -name "libfoo.so*"`.
2. Jeśli go nie ma pod oczywistą nazwą, **nie zakładaj, że go nie ma w
   ogóle** — sprawdź `/etc/ld.so.conf.d/*.conf` i `ldconfig -p | grep
   libfoo`. Biblioteka może być zainstalowana pod nietypową ścieżką
   (np. `pipewire-jack-audio-connection-kit-libs` instaluje `libjack.so.0`
   w `/usr/lib64/pipewire-0.3/jack/`, ale ta ścieżka jest zarejestrowana w
   `ld.so.conf.d` i linker normalnie ją widzi — bez żadnego wrappera).
   Założenie "tego nie ma / linker by tego nie znalazł" bez sprawdzenia
   `ld.so.conf.d` to najdroższy błąd w tej metodologii — prowadzi do
   pakowania czegoś zbędnego, co potem trzeba wycofywać.
3. Sprawdź `rpm -qf <plik>` i `rpm -q --whatprovides "biblioteka()(64bit)"`
   — to pokazuje, co jest zarejestrowane w bazie RPM (czyli w obrazie
   ostree lub warstwie rpm-ostree), niezależnie od tego, co aktualnie
   "widać" w zmergowanym `/usr/lib64` (bo TO może akurat pokazywać Twój
   WŁASNY, już aktywny sysext, a nie bazę).
4. `rpm-ostree status` pokazuje realnie warstwowane pakiety — porównaj z
   tym, co zamierzasz pakować, żeby nie dublować czegoś, co już jest
   warstwą.

Dopiero gdy żadna z tych ścieżek nic nie znajduje, biblioteka faktycznie
jest kandydatem do spakowania.

## 2. Mechanika obrazu

```bash
mkdir -p usr/lib/extension-release.d
printf 'ID=_any\nARCHITECTURE=x86-64\nSYSEXT_SCOPE=system\n' \
  > usr/lib/extension-release.d/extension-release.<nazwa>

mkfs.erofs -zlz4hc --all-root \
  --file-contexts=/etc/selinux/targeted/contexts/files/file_contexts \
  <nazwa>.raw build   # <- katalog źródłowy to RODZIC 'usr/', NIE 'usr/' samo
```

Pułapki:
- Źródło dla `mkfs.erofs` musi być katalogiem zawierającym `usr/`, nie
  samym `usr/` — inaczej `extension-release.d` jest niewidoczne i merge
  po cichu nie obejmuje tego rozszerzenia.
- `ID=fedora`/`ID=bazzite` w `extension-release` nie zawsze dopasowuje się
  niezawodnie do hosta (zależnie od dokładnej wartości `/etc/os-release`).
  `ID=_any` jest bezpieczniejszym domyślnym wyborem, jeśli nie potrzebujesz
  twardej bramki kompatybilności.
- Sysext ma zawierać WYŁĄCZNIE `/usr` (i ew. `/opt`) — żadnych plików w
  `/etc`, `/var` itd. Jeśli rozpakowanie RPM-a zostawi coś poza `/usr`
  (typowo: `/etc/security/limits.d/`, `/etc/fonts/conf.d/`, gołe `/lib/...`
  bez prefiksu `usr/`), trzeba to albo usunąć, albo przenieść pod `usr/`.
- `file_contexts` bierz z HOSTA, na którym budujesz (etykiety SELinux) —
  ale pamiętaj, że to przywiązuje obraz do polityki SELinux TEGO hosta;
  na systemie bez SELinux jest to nieszkodliwe, na innym z INNĄ polityką
  może dawać złe etykiety.

## 3. Weryfikacja zależności — rzetelnie, nie przez zgadywanie

**Nigdy nie używaj `strings`/`grep` po nazwie biblioteki jako dowodu
zależności.** Plik `libfoo.so.1` zawiera własną nazwę w swoim SONAME —
naiwny `strings plik | grep libfoo` da fałszywe trafienie na SAMEGO
SIEBIE. Właściwe narzędzie:

```bash
readelf -d <plik> | grep NEEDED
```

To pokazuje PRAWDZIWE, twarde zależności linkowania. Do sprawdzenia "kto
na całym żywym systemie potrzebuje biblioteki X, poza moim własnym
pakietem":

```bash
for f in /usr/bin/* /usr/lib64/*.so*; do
    [ -f "$f" ] || continue
    bn=$(basename "$f" | tr 'A-Z' 'a-z')
    grep -qxF "$bn" /tmp/moje_wlasne_pliki.txt && continue   # wyklucz WŁASNE pliki (w tym symlinki, case-insensitive!)
    readelf -d "$f" 2>/dev/null | grep NEEDED | grep -q "libfoo.so" && echo "$f potrzebuje libfoo"
done
```

Pułapka nr 1: lista "własnych plików" do wykluczenia musi zawierać
symlinki (`find ... -type f -o -type l`, nie tylko `-type f`) i musi być
case-insensitive — inaczej złapiesz fałszywe alarmy typu "moja własna
biblioteka X potrzebuje mojej własnej biblioteki Y" jako "obcy konsument".

Pułapka nr 2: `ldd`/`readelf` na maszynie, gdzie Twój WŁASNY sysext jest
już aktywny, zawsze zobaczy zmergowany, aktywny stan — łącznie z Twoimi
własnymi plikami pod `/usr/lib64`. Jeśli sprawdzasz "czy coś obcego z tego
korzysta", musisz precyzyjnie wykluczyć wszystko, co Twoje, żeby nie
porównywać obrazu z nim samym.

Realny przykład z tego projektu: pierwsze podejście (przez `strings`)
pokazało dziesiątki fałszywych "konsumentów". Po przejściu na `readelf -d
NEEDED` z poprawnym wykluczeniem okazało się, że 15 z 16 bibliotek nie ma
ŻADNEGO obcego konsumenta, a jedna (`libjack.so.0`) ma trzech prawdziwych
(inny program z osobnego sysextu, `libavdevice` z Fedory, `libportaudio`
z Fedory) — i ostatecznie ta jedna w ogóle nie powinna być pakowana, bo
punkt 1 (sprawdź `ld.so.conf.d`) pokazał, że baza już ją dostarcza.

## 4. Izolacja bibliotek: prywatny RUNPATH per pakiet (wzorzec AppImage)

Dla bibliotek, które SĄ Twoje (baza ich nie dostarcza, punkt 1 to
potwierdził) i które NIE mają obcych konsumentów (punkt 3 to potwierdził)
— warto je odizolować, żeby przyszła aktualizacja bazy nigdy nie mogła ich
podmienić i żeby żaden inny program nigdy przypadkiem nie zaczął z nich
korzystać:

```bash
mkdir -p usr/lib/<pakiet>
mv usr/lib64/libfoo.so* usr/lib/<pakiet>/
patchelf --set-rpath '$ORIGIN/../lib/<pakiet>' usr/bin/konsument
```

Krytyczna pułapka — **RUNPATH nie jest dziedziczony tranzytywnie** (w
odróżnieniu od starego RPATH). Jeśli `libfoo.so` w Twoim prywatnym
katalogu sama zależy od `libbar.so` w TYM SAMYM katalogu, ustawienie
RUNPATH tylko na konsumującej binarce NIE WYSTARCZY — dynamiczny linker
rozwiąże zależność `libfoo→libbar` przez RUNPATH samej `libfoo.so`, które
domyślnie jest puste. Trzeba dodatkowo:

```bash
find usr/lib/<pakiet> -name "*.so*" -exec patchelf --set-rpath '$ORIGIN' {} \;
```

na KAŻDYM pliku wewnątrz katalogu (self-referential RUNPATH — "szukaj też
w moim własnym katalogu"). Bez tego kroku pierwsza warstwa zależności
zadziała, a druga (tranzytywna) po cichu spadnie z powrotem na
`/usr/lib64` bazy — dokładnie ten błąd, który przeszedł pierwszą,
niedokładną weryfikację w tym projekcie.

Inne pułapki po drodze:
- Symlinki wersji SONAME (np. `libfoo.so.1 -> libfoo.so.1.2.3`) trzeba
  przenosić OBIE strony razem — łatwo zapomnieć o samym symlinku albo o
  pliku docelowym, zostawiając martwy link. Po KAŻDYM przenoszeniu:
  `find . -xtype l` (zero wyników = OK).
- Katalog `usr/lib/.build-id/` (symlinki debug-lookup po hashu) staje się
  częściowo martwy po przeniesieniu bibliotek — nieszkodliwe dla działania
  (tylko debugger/coredumpctl), ale warto posprzątać: `find . -xtype l
  -delete`.
- Pluginy ładowane przez `dlopen()` (np. plugin Hyprlanda, pluginy Qt6)
  TEŻ potrzebują własnego RUNPATH, liczonego względem ICH położenia w
  drzewie (głębiej zagnieżdżone = więcej `../` w `$ORIGIN/../../../lib/...`).
  W praktyce i tak reużyją bibliotek już załadowanych przez proces
  macierzysty dla tych samych SONAME, ale RUNPATH pluginu nadal się liczy
  dla JEGO WŁASNEGO rozwiązywania zależności.

To NIE rozwiązuje przenośności na inne dystrybucje/systemy — to wyłącznie
higiena w obrębie tej samej linii dystrybucji (patrz punkt 6).

## 5. Dopasowanie ABI dla bibliotek z niestabilnym prywatnym API

Niektóre biblioteki na hoście (typowo Qt6, jeśli cokolwiek używa jego
prywatnego API) NIE gwarantują stabilności ABI między wersjami, nawet
punktowymi. Jeśli budowana przez Ciebie binarka linkuje się z prywatnym
API takiej biblioteki, **budowanie przeciw "jakiejkolwiek" wersji z
COPR/innego builda nie wystarczy** — trzeba budować przeciw DOKŁADNIE tej
wersji, która jest na docelowym hoście, inaczej dostaniesz `undefined
symbol` przy starcie.

Przepis (bez dotykania hosta, bez roota):

```bash
# 1. Ustal dokładne NVR na hoście
rpm -q qt6-qtbase qt6-qtdeclarative ...

# 2. Pobierz DOKŁADNIE te same NVR (-devel ORAZ runtime, bo symlinki
#    z -devel wskazują na konkretne nazwy plików runtime)
dnf5 download qt6-qtbase-devel-<dokładna-wersja> --destdir=. --arch=x86_64
dnf5 download qt6-qtbase-<dokładna-wersja> --destdir=. --arch=x86_64
# ... (wszystkie zależności ze speca, ich transytywne -devel też)

# 3. Rozpakuj do izolowanego sysrootu (rpm2cpio, NIE instaluj na hosta)
mkdir sysroot && cd sysroot
for rpm in ../rpms/*.rpm; do rpm2cpio "$rpm" | cpio -idm; done

# 4. Buduj z jawnym sysrootem, NIE pozwalając cmake "znaleźć" niczego
#    poza nim (patrz pułapki niżej)
export PKG_CONFIG_SYSROOT_DIR="$SYSROOT_DIR"
export PKG_CONFIG_PATH="$SYSROOT/lib64/pkgconfig:$SYSROOT/share/pkgconfig"
cmake -DCMAKE_C_COMPILER=/usr/bin/gcc -DCMAKE_CXX_COMPILER=/usr/bin/g++ \
      -DCMAKE_C_FLAGS="-isystem $SYSROOT/include" \
      -DCMAKE_CXX_FLAGS="-isystem $SYSROOT/include" \
      -DCMAKE_EXE_LINKER_FLAGS="-L$SYSROOT/lib64" ...
```

Pułapki:
- `.pc` pliki mają `prefix=/usr` zaszyty na sztywno — bez
  `PKG_CONFIG_SYSROOT_DIR` cmake "znajduje" ścieżki, które fizycznie nie
  istnieją (prawdziwy `/usr` hosta, nie sysroot).
- Ustawienie `CMAKE_PREFIX_PATH=$SYSROOT` ma efekt uboczny: `find_program()`
  zaczyna preferować `cmake`/`ninja`/`gcc` Z SYSROOTU (zwykle niedziałające,
  bo niekompletne) zamiast działających binarek hosta. Rozwiązanie: jawnie
  wymuszać `CMAKE_C_COMPILER`/`CMAKE_CXX_COMPILER`/`CMAKE_MAKE_PROGRAM` na
  ścieżki hosta, nie polegać na automatycznym wykryciu.
- Część `.pc` nie emituje `-I`/`-L` dla ścieżek uznawanych przez kompilator
  za domyślne — potrzebne globalne `-isystem $SYSROOT/include` i
  `-L$SYSROOT/lib64` w fladze, niezależnie od tego, co mówi pkg-config.
- Symlinki `-devel` czasami wskazują na plik runtime, którego nie ma w
  sysroocie (bo pobrałeś tylko `-devel`, nie runtime tej samej wersji) —
  zawsze pobieraj OBA w tej samej wersji.

Wynikowa binarka ma SONAME identyczne z paczką dystrybucyjną, więc
podmienia się bez przebudowy reszty stosu.

**To dopasowanie jest ważne tylko dopóki baza się nie zmieni.** Po
aktualizacji bazy (nowa wersja Qt6) może być potrzebne powtórzenie całego
przepisu z nowymi NVR-ami. To nieuniknione przy używaniu prywatnego API —
nie próbuj tego "raz na zawsze" zabezpieczyć, tylko miej przepis gotowy do
szybkiego powtórzenia (patrz punkt 7).

## 6. Granice tego podejścia — czego to NIE rozwiązuje

- **Sterownik GPU jest twardym sufitem.** Kompozytor Wayland rozmawia
  bezpośrednio z DRM/KMS jądra i konkretnym sterownikiem (Mesa albo
  właścicielski). Nie da się tego "zapakować w środek" sysextu — musi się
  zgadzać z tym, co faktycznie załadowane w jądrze hosta.
- **`systemd-sysext` wymaga systemd.** Zero przenośności na dystrybucje
  bez systemd (Alpine, Void, Gentoo-OpenRC) i oczywiście na nie-Linux.
- **Zakładany stos bazowy musi być obecny.** Jeśli sysext świadomie NIE
  dubluje Qt6/Wayland/Mesa/PipeWire (żeby nie rozdymać obrazu), to
  wymaga hosta, który już to ma — nie zadziała na spinie bez tego stosu
  (np. GNOME-owy Silverblue dla obrazu zależnego od KDE/Qt6).
- Żadna technika z tego dokumentu (RUNPATH, dopasowanie NVR) nie zmienia
  tych trzech granic — to są granice architektoniczne, nie luki w
  implementacji.

## 7. Dyscyplina procesu

- **Buduj do pliku roboczego, nie nadpisuj od razu aktywnego
  `/var/lib/extensions/<nazwa>.raw`.** Sprawdź `systemd-sysext status` i
  `ls /var/lib/extensions/`, żeby wiedzieć, czy plik, który nadpisujesz,
  jest właśnie aktywnie zmergowany.
- **Weryfikuj przez narzędzia, nie przez pamięć** — szczególnie fakty
  specyficzne dla dystrybucji/wersji (czy coś jest warstwowane przez
  rpm-ostree, jaka jest dokładna wersja pakietu, czy dana ścieżka jest w
  `ld.so.conf.d`). Założenia bez sprawdzenia to najdroższe błędy w całej
  tej metodologii.
- **`luac -p plik.lua`** (albo odpowiednik dla innego języka configu) do
  czystej weryfikacji składni bez żadnych efektów ubocznych — zero ryzyka
  dla żywego systemu.
- **Polecenia, które WIDOCZNIE ruszają żywy, używany system (kursor,
  fokus okna, przełączanie pulpitów, restart usług) to nie to samo co
  bezpieczny odczyt** (`hyprctl monitors -j`, `rpm -qf`, `readelf -d`).
  Jeśli coś zmienia to, co użytkownik aktualnie widzi/robi na ekranie —
  zapytaj najpierw, nie zakładaj że krótka chwila zakłócenia jest
  nieszkodliwa.
- Trzymaj dokładne wersje pakietów przypięte w jednym miejscu (plik
  manifestu albo sekcja w README) — "odtwórz obraz" powinno być
  wykonaniem jednego udokumentowanego przepisu, nie odtwarzaniem sesji
  czatu z pamięci.
