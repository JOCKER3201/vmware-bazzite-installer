# How to build systemd-sysext images (`.raw`) — methodology and pitfalls

This document is distilled from real work on `dms-fedora-sysext` (Hyprland +
DankMaterialShell on Bazzite/Kinoite). It is addressed to an AI building
similar sysext images — every rule here is backed by a concrete incident from
that project, none of it is theory.

## 1. The fundamental rule: check the base before adding anything

A sysext only makes sense when it does NOT duplicate what the host already
has. Before packing any library/binary into the image:

1. Check whether a file with the same name (SONAME) already exists on the
   host: `find /usr/lib64 -name "libfoo.so*"`.
2. If it is not there under the obvious name, **do not assume it is absent
   altogether** — check `/etc/ld.so.conf.d/*.conf` and `ldconfig -p | grep
   libfoo`. The library may be installed under an unusual path
   (e.g. `pipewire-jack-audio-connection-kit-libs` installs `libjack.so.0`
   in `/usr/lib64/pipewire-0.3/jack/`, but that path is registered in
   `ld.so.conf.d` and the linker sees it normally — without any wrapper).
   Assuming "it's not there / the linker would not find it" without checking
   `ld.so.conf.d` is the most expensive mistake in this methodology — it
   leads to packing something unnecessary that later has to be rolled back.
3. Check `rpm -qf <file>` and `rpm -q --whatprovides "library()(64bit)"`
   — this shows what is registered in the RPM database (i.e. in the ostree
   image or an rpm-ostree layer), regardless of what is currently "visible"
   in the merged `/usr/lib64` (because THAT may well be showing your OWN,
   already active sysext rather than the base).
4. `rpm-ostree status` shows the actually layered packages — compare it with
   what you intend to pack, so you do not duplicate something that is
   already a layer.

Only when none of these paths finds anything is the library actually a
candidate for packing.

## 2. Image mechanics

```bash
mkdir -p usr/lib/extension-release.d
printf 'ID=_any\nARCHITECTURE=x86-64\nSYSEXT_SCOPE=system\n' \
  > usr/lib/extension-release.d/extension-release.<name>

mkfs.erofs -zlz4hc --all-root \
  --file-contexts=/etc/selinux/targeted/contexts/files/file_contexts \
  <name>.raw build   # <- the source directory is the PARENT of 'usr/', NOT 'usr/' itself
```

Pitfalls:
- The source for `mkfs.erofs` must be the directory containing `usr/`, not
  `usr/` itself — otherwise `extension-release.d` is invisible and the merge
  silently skips the extension.
- `ID=fedora`/`ID=bazzite` in `extension-release` does not always match the
  host reliably (depending on the exact value of `/etc/os-release`).
  `ID=_any` is the safer default choice if you do not need a hard
  compatibility gate.
- A sysext must contain ONLY `/usr` (and possibly `/opt`) — no files in
  `/etc`, `/var` etc. If unpacking an RPM leaves something outside `/usr`
  (typically: `/etc/security/limits.d/`, `/etc/fonts/conf.d/`, bare
  `/lib/...` without the `usr/` prefix), it has to be either removed or
  moved under `usr/`.
- Take `file_contexts` from the HOST you are building on (SELinux labels) —
  but remember this ties the image to THAT host's SELinux policy;
  on a system without SELinux it is harmless, on another one with a
  DIFFERENT policy it may produce wrong labels.

## 3. Dependency verification — rigorously, not by guessing

**Never use `strings`/`grep` on a library name as proof of a dependency.**
The file `libfoo.so.1` contains its own name in its SONAME — a naive
`strings file | grep libfoo` produces a false hit on ITSELF. The right tool:

```bash
readelf -d <file> | grep NEEDED
```

This shows the REAL, hard link-time dependencies. To check "who on the whole
live system needs library X, apart from my own package":

```bash
for f in /usr/bin/* /usr/lib64/*.so*; do
    [ -f "$f" ] || continue
    bn=$(basename "$f" | tr 'A-Z' 'a-z')
    grep -qxF "$bn" /tmp/my_own_files.txt && continue   # exclude YOUR OWN files (including symlinks, case-insensitive!)
    readelf -d "$f" 2>/dev/null | grep NEEDED | grep -q "libfoo.so" && echo "$f needs libfoo"
done
```

Pitfall no. 1: the "own files" exclusion list must include symlinks
(`find ... -type f -o -type l`, not just `-type f`) and must be
case-insensitive — otherwise you get false alarms of the kind "my own
library X needs my own library Y" reported as a "foreign consumer".

Pitfall no. 2: `ldd`/`readelf` on a machine where your OWN sysext is
already active always sees the merged, active state — including your own
files under `/usr/lib64`. If you are checking "does anything foreign use
this", you must precisely exclude everything of yours, so you do not end up
comparing the image with itself.

A real example from this project: the first attempt (via `strings`) showed
dozens of false "consumers". After switching to `readelf -d NEEDED` with
correct exclusion it turned out that 15 of the 16 libraries have NO foreign
consumer at all, and one (`libjack.so.0`) has three real ones (another
program from a separate sysext, `libavdevice` from Fedora, `libportaudio`
from Fedora) — and in the end that one should not have been packed at all,
because step 1 (check `ld.so.conf.d`) showed the base already provides it.

## 4. Library isolation: a private RUNPATH per package (the AppImage pattern)

For libraries that ARE yours (the base does not provide them, step 1
confirmed that) and that have NO foreign consumers (step 3 confirmed that) —
it is worth isolating them, so that a future base update can never replace
them and no other program ever accidentally starts using them:

```bash
mkdir -p usr/lib/<package>
mv usr/lib64/libfoo.so* usr/lib/<package>/
patchelf --set-rpath '$ORIGIN/../lib/<package>' usr/bin/consumer
```

The critical pitfall — **RUNPATH is not inherited transitively** (unlike the
old RPATH). If `libfoo.so` in your private directory itself depends on
`libbar.so` in the SAME directory, setting RUNPATH only on the consuming
binary is NOT ENOUGH — the dynamic linker resolves the `libfoo→libbar`
dependency through the RUNPATH of `libfoo.so` itself, which is empty by
default. You additionally need:

```bash
find usr/lib/<package> -name "*.so*" -exec patchelf --set-rpath '$ORIGIN' {} \;
```

on EVERY file inside the directory (a self-referential RUNPATH — "also look
in my own directory"). Without this step the first layer of dependencies
works, while the second (transitive) one silently falls back to the base's
`/usr/lib64` — exactly the bug that slipped through the first, sloppy
verification in this project.

Other pitfalls along the way:
- SONAME version symlinks (e.g. `libfoo.so.1 -> libfoo.so.1.2.3`) must be
  moved with BOTH sides together — it is easy to forget either the symlink
  itself or the target file, leaving a dead link. After EVERY move:
  `find . -xtype l` (zero results = OK).
- The `usr/lib/.build-id/` directory (debug-lookup symlinks by hash) becomes
  partially dead after moving libraries — harmless for operation (only the
  debugger/coredumpctl), but worth cleaning up: `find . -xtype l -delete`.
- Plugins loaded via `dlopen()` (e.g. a Hyprland plugin, Qt6 plugins) ALSO
  need their own RUNPATH, computed relative to THEIR location in the tree
  (nested deeper = more `../` in `$ORIGIN/../../../lib/...`).
  In practice they will reuse the libraries already loaded by the parent
  process for the same SONAMEs, but the plugin's RUNPATH still matters for
  resolving ITS OWN dependencies.
- This does NOT solve portability to other distributions/systems — it is
  purely hygiene within the same distribution line (see item 6).

## 5. ABI matching for libraries with an unstable private API

Some libraries on the host (typically Qt6, if anything uses its private
API) do NOT guarantee ABI stability between versions, even point releases.
If a binary you build links against the private API of such a library,
**building against "any" version from COPR/another build is not enough** —
you have to build against EXACTLY the version present on the target host,
otherwise you get `undefined symbol` at startup.

The recipe (without touching the host, without root):

```bash
# 1. Determine the exact NVRs on the host
rpm -q qt6-qtbase qt6-qtdeclarative ...

# 2. Download EXACTLY those NVRs (-devel AND runtime, because the symlinks
#    from -devel point at specific runtime file names)
dnf5 download qt6-qtbase-devel-<exact-version> --destdir=. --arch=x86_64
dnf5 download qt6-qtbase-<exact-version> --destdir=. --arch=x86_64
# ... (all dependencies from the spec, their transitive -devel too)

# 3. Unpack into an isolated sysroot (rpm2cpio, do NOT install on the host)
mkdir sysroot && cd sysroot
for rpm in ../rpms/*.rpm; do rpm2cpio "$rpm" | cpio -idm; done

# 4. Build with an explicit sysroot, NOT letting cmake "find" anything
#    outside it (see the pitfalls below)
export PKG_CONFIG_SYSROOT_DIR="$SYSROOT_DIR"
export PKG_CONFIG_PATH="$SYSROOT/lib64/pkgconfig:$SYSROOT/share/pkgconfig"
cmake -DCMAKE_C_COMPILER=/usr/bin/gcc -DCMAKE_CXX_COMPILER=/usr/bin/g++ \
      -DCMAKE_C_FLAGS="-isystem $SYSROOT/include" \
      -DCMAKE_CXX_FLAGS="-isystem $SYSROOT/include" \
      -DCMAKE_EXE_LINKER_FLAGS="-L$SYSROOT/lib64" ...
```

Pitfalls:
- `.pc` files have `prefix=/usr` hardcoded — without
  `PKG_CONFIG_SYSROOT_DIR` cmake "finds" paths that do not physically exist
  (the host's real `/usr`, not the sysroot).
- Setting `CMAKE_PREFIX_PATH=$SYSROOT` has a side effect: `find_program()`
  starts preferring the `cmake`/`ninja`/`gcc` FROM THE SYSROOT (usually
  non-functional, because incomplete) over the host's working binaries.
  The solution: explicitly force `CMAKE_C_COMPILER`/`CMAKE_CXX_COMPILER`/
  `CMAKE_MAKE_PROGRAM` to host paths, do not rely on autodetection.
- Some `.pc` files do not emit `-I`/`-L` for paths the compiler considers
  default — you need a global `-isystem $SYSROOT/include` and
  `-L$SYSROOT/lib64` in the flags, regardless of what pkg-config says.
- `-devel` symlinks sometimes point at a runtime file that is not in the
  sysroot (because you downloaded only `-devel`, not the runtime of the same
  version) — always download BOTH in the same version.

The resulting binary has a SONAME identical to the distribution package, so
it swaps in without rebuilding the rest of the stack.

**This matching only holds until the base changes.** After a base update
(a new Qt6 version) the whole recipe may need to be repeated with the new
NVRs. This is unavoidable when using a private API — do not try to secure
it "once and for all"; instead keep the recipe ready for a quick re-run
(see item 7).

## 6. The limits of this approach — what it does NOT solve

- **The GPU driver is a hard ceiling.** A Wayland compositor talks directly
  to the kernel's DRM/KMS and to the specific driver (Mesa or proprietary).
  This cannot be "packed inside" a sysext — it must match what is actually
  loaded in the host kernel.
- **`systemd-sysext` requires systemd.** Zero portability to distributions
  without systemd (Alpine, Void, Gentoo-OpenRC), and obviously to non-Linux.
- **The assumed base stack must be present.** If the sysext deliberately
  does NOT duplicate Qt6/Wayland/Mesa/PipeWire (to avoid bloating the
  image), it requires a host that already has them — it will not work on a
  spin without that stack (e.g. a GNOME Silverblue for an image that depends
  on KDE/Qt6).
- No technique in this document (RUNPATH, NVR matching) changes these three
  limits — they are architectural boundaries, not implementation gaps.

## 7. Process discipline

- **Build to a scratch file; do not immediately overwrite the active
  `/var/lib/extensions/<name>.raw`.** Check `systemd-sysext status` and
  `ls /var/lib/extensions/` to know whether the file you are overwriting is
  actively merged right now.
- **Verify with tools, not from memory** — especially distribution/version
  specific facts (whether something is layered by rpm-ostree, what the exact
  package version is, whether a given path is in `ld.so.conf.d`). Unchecked
  assumptions are the most expensive mistakes in this whole methodology.
- **`luac -p file.lua`** (or the equivalent for another config language) for
  a pure syntax check with no side effects — zero risk to the live system.
- **Commands that VISIBLY disturb a live, in-use system (the cursor, window
  focus, desktop switching, service restarts) are not the same as a safe
  read** (`hyprctl monitors -j`, `rpm -qf`, `readelf -d`). If something
  changes what the user currently sees/does on screen — ask first, do not
  assume a brief moment of disruption is harmless.
- Keep the exact package versions pinned in one place (a manifest file or a
  README section) — "reproduce the image" should be the execution of a
  single documented recipe, not a reconstruction of a chat session from
  memory.
