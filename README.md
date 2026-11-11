# vmware-sysext-builder

A graphical wizard that takes the official **VMware Workstation for Linux
installer (`.bundle`)** and builds a ready-to-use **`vmware.raw`** image for
**systemd-sysext** — intended for **Bazzite** and other Fedora Atomic systems
(read-only system image). Written entirely in Rust (egui).

> ⚠ **This is NOT an official VMware installer.** An unofficial project,
> in no way affiliated with Broadcom or VMware — use at your own risk.
>
> 🤖 **Created by an AI** (Claude, Anthropic) commissioned and supervised
> by the repository owner.

## What it does

0. **Detects the system and kernel version on its own.** On startup it reads
   the manifest of the installed image (`/usr/share/vmware-sysext/manifest`)
   and compares it with the running kernel. After a system update (e.g.
   Bazzite F44 → F45) it shows a banner, locates the previously used
   `.bundle` package (the path from the manifest or the download directory)
   and rebuilds the image for the current kernel **with a single click** —
   no manual picking of anything.
1. **You select the `.bundle` file** — downloaded yourself from the Broadcom
   portal (since November 2024 Workstation Pro is free, including for
   commercial use; the program downloads nothing from the network). When
   rebuilding after an update, the file is already detected automatically.
2. **It detects the installed kernels** (`/usr/lib/modules`) and checks for
   headers (`kernel-devel`); it selects the running kernel by default and
   skips leftovers of merged extensions (directories containing only the
   vmmon/vmnet modules of an old kernel).
   Bazzite ships `gcc`, `make` and a matching `kernel-devel` in the base image.
3. After clicking **"Build vmware.raw"** it:
   - extracts the `.bundle` with `sh <bundle> --extract` (without installing
     anything and without root),
   - **compiles the `vmmon` and `vmnet` kernel modules** for the selected
     kernel version (`make VM_UNAME=<version>`); optionally from patched
     sources or with the AUR patches (see below),
   - assembles the `/usr` tree following the proven layout of the AUR
     `vmware-workstation` package (binaries, libraries, appLoader symlinks,
     setuid bits, .desktop files, systemd units),
   - disables the module check (`vmware-modconfig`) in the launcher, which
     leads nowhere on a system with a read-only `/usr`,
   - generates a **superset of `depmod` metadata** (base modules + vmmon/vmnet),
     so that after the merge plain `modprobe` works and nothing shadows the
     base modules,
   - **verifies the tree before packing**: structure (only `/usr`), presence
     of `extension-release`, dangling symlinks, and module `vermagic`
     matching the target kernel (`modinfo`),
   - writes a **reproducibility manifest** (`/usr/share/vmware-sysext/manifest`
     inside the image + a `vmware-sysext-manifest.txt` copy next to it):
     package name, path and SHA-256, VMware version, kernel, system
     (`os-release`), origin of the module sources, build date — this is what
     the autodetection uses on subsequent system updates,
   - builds an **erofs** image with SELinux labels from the system
     `file_contexts` (stock Bazzite runs in enforcing mode).
4. It shows the installation commands, or installs right away via `pkexec`.

## Installing the image

The program shows these commands after a successful build (and can run them
itself):

```sh
sudo install -D -m 0644 vmware.raw /var/lib/extensions/vmware.raw
sudo restorecon -RF /var/lib/extensions
sudo systemd-sysext refresh --no-reload
sudo systemd-tmpfiles --create /usr/lib/tmpfiles.d/vmware-sysext.conf
sudo systemctl daemon-reload
```

The order matters: `--no-reload` defers the service startup until `tmpfiles`
has seeded `/etc/vmware` — only `daemon-reload` starts the services (the
`Upholds=` drop-in).

That is all — the image ships an `Upholds=` drop-in for `multi-user.target`,
so the services (module loading, vmnet networks, the USB arbitrator) start
automatically after the merge and on every boot. `systemd-sysext.service`
is enabled by default on Bazzite, so the merge survives a reboot.
The `/etc/vmware` directory is seeded by `tmpfiles.d` (type `C+` — it never
overwrites your changes). After that, just run `vmware`.

Uninstalling:

```sh
sudo rm /var/lib/extensions/vmware.raw
sudo systemd-sysext refresh
```

## Building the program

```sh
make build        # compile (cargo build --release), installs nothing
make install      # per-user installation: ~/.local/bin + menu entry
make uninstall    # remove what `make install` installed
make package      # self-contained AppImage (requires appimagetool in PATH)
```

Or directly:

```sh
cargo build --release
./target/release/vmware-sysext-builder
```

## Version compatibility and patches

- **Recommended packages: 25H2 / 26H1** (Broadcom's new year-based naming) —
  their module sources build without patches on 7.x kernels.
- **17.6.x packages do not compile** on kernels ≥ 6.15 without community
  patches; for 7.x kernels a 25H2/26H1 package is practically required.
- When compilation fails, point "Advanced options" at a directory:
  - with the **sources of a `vmware-host-modules` fork** (`vmmon-only/` and
    `vmnet-only/` layout), e.g. `philipl/vmware-host-modules` (branches
    `workstation-25h2`, `workstation-26h1`), or
  - with the **patches** `vmmon.patch` / `vmnet.patch` (e.g. a checkout of
    the AUR `vmware-workstation` package — mirror: `github.com/archlinux/aur`,
    branch `vmware-workstation`) — they will be applied with `patch -p2`
    to the package sources.
- The modules are coupled to the product version (they refuse to load on a
  mismatch) — use sources/patches for the same version as the `.bundle`.

## Important notes

- **SELinux labels:** when building the labeled image fails, the program
  aborts with an error on a system in enforcing mode (an unlabeled image
  would not work anyway); on permissive it builds an unlabeled fallback
  image and clearly flags that in the result panel.
- **A kernel update** (rpm-ostree / bootc) invalidates the modules — after
  every Bazzite update run the program: it detects the mismatch and rebuilds
  the image with one click. The `vmware-modules.service` unit has a condition
  on the exact kernel version, so until the rebuild it simply does not start
  (no errors at boot).
- **Secure Boot:** unsigned modules will not load with Secure Boot enabled.
  The Universal Blue key (`akmods-ublue`) cannot sign locally built modules
  (the private part stays in CI) — you need your own MOK key enrolled with
  `mokutil --import`. The program can sign the modules with the key and
  certificate you point it at (advanced options).
- The image **does not include** the `vmware-installer`, VIX, ovftool or
  vprobe components (not needed for everyday use; smaller image).
- The program **neither downloads nor distributes** VMware software — the
  image is built locally from a file supplied by the user and contains
  software covered by the Broadcom/VMware license; do not redistribute it.
- VMware and VMware Workstation are trademarks of Broadcom Inc. This project
  is not affiliated with Broadcom.

## License

The code of this tool: [MIT](LICENSE). The installation layout was
reconstructed from the facts of the AUR `vmware-workstation` package and the
NixOS package (no code copied); the sysext patterns follow the
fedora-sysexts and Flatcar projects.
