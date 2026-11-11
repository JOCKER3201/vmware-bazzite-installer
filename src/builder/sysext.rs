// Assembly of the systemd-sysext extension tree (/usr), computation of module
// metadata (depmod — superset of base + ours) and building of the vmware.raw image.
//
// The component mapping mirrors the proven manual installation layout
// from the AUR vmware-workstation package (independently confirmed in NixOS).

use anyhow::{bail, Context, Result};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::builder::{assets, modules::BuiltModules, BuildConfig, BuildEvent};
use crate::util;

/// Mapping: (component name, subdirectory/file within the component, destination directory).
/// The contents of a source directory are merged into the destination; a single
/// file goes inside the destination directory.
const MAP_RULES: &[(&str, &str, &str)] = &[
    // main application
    ("vmware-workstation", "bin", "usr/bin"),
    ("vmware-workstation", "lib", "usr/lib/vmware"),
    ("vmware-workstation", "share", "usr/share"),
    ("vmware-workstation", "man", "usr/share/man"),
    ("vmware-workstation", "doc", "usr/share/doc/vmware-workstation"),
    // extra applications: "vmware-other-apps" in 25H2+, "vmware-player-app" in 17.6.x
    ("vmware-other-apps", "bin", "usr/bin"),
    ("vmware-other-apps", "lib", "usr/lib/vmware"),
    ("vmware-other-apps", "share", "usr/share"),
    ("vmware-other-apps", "doc", "usr/share/doc/vmware-workstation"),
    ("vmware-player-app", "bin", "usr/bin"),
    ("vmware-player-app", "lib", "usr/lib/vmware"),
    ("vmware-player-app", "share", "usr/share"),
    ("vmware-player-app", "doc", "usr/share/doc/vmware-workstation"),
    ("vmware-player-setup", "vmware-config", "usr/lib/vmware/setup"),
    // virtual machine engine (sbin → /usr/bin: that is where vmware-authd lives)
    ("vmware-vmx", "bin", "usr/bin"),
    ("vmware-vmx", "sbin", "usr/bin"),
    ("vmware-vmx", "lib", "usr/lib/vmware"),
    ("vmware-vmx", "roms", "usr/lib/vmware/roms"),
    // network editor and USB arbitrator (the editor's lib merges into
    // /usr/lib/vmware/lib, not /usr/lib/vmware — this is what AUR and NixOS do)
    ("vmware-network-editor", "lib", "usr/lib/vmware/lib"),
    ("vmware-network-editor-ui", "share", "usr/share"),
    ("vmware-usbarbitrator", "bin", "usr/lib/vmware/bin"),
];

/// Components deliberately omitted from the minimal image.
const SKIPPED_COMPONENTS: &[&str] = &[
    "vmware-installer",
    "vmware-vix-core",
    "vmware-ovftool",
    "vmware-vprobe",
];

/// Files with the setuid bit — exactly as in the official VMware installation.
const SUID_BINARIES: &[&str] = &[
    "usr/bin/vmware-authd",
    "usr/lib/vmware/bin/vmware-vmx",
    "usr/lib/vmware/bin/vmware-vmx-debug",
    "usr/lib/vmware/bin/vmware-vmx-stats",
];

/// Names in /usr/lib/vmware/bin that are symlinks to appLoader
/// (appLoader recognizes the program by argv[0]).
const APPLOADER_LINKS: &[&str] = &[
    "licenseTool",
    "vmware",
    "vmware-app-control",
    "vmware-enter-serial",
    "vmware-fuseUI",
    "vmware-gksu",
    "vmware-modconfig",
    "vmware-modconfig-console",
    "vmware-mount",
    "vmware-netcfg",
    "vmware-setup-helper",
    "vmware-tray",
    "vmware-vmblock-fuse",
    "vmware-vprobe",
    "vmware-zenity",
];

/// Symlinks in /usr/bin.
const USR_BIN_LINKS: &[(&str, &str)] = &[
    ("vmrest", "/usr/lib/vmware/bin/appLoader"),
    ("vmware-fuseUI", "/usr/lib/vmware/bin/vmware-fuseUI"),
    ("vmware-mount", "/usr/lib/vmware/bin/vmware-mount"),
    ("vmware-netcfg", "/usr/lib/vmware/bin/vmware-netcfg"),
    ("vmware-usbarbitrator", "/usr/lib/vmware/bin/vmware-usbarbitrator"),
];

/// Fills in @@BINARY@@ in .desktop files.
const DESKTOP_BINARIES: &[(&str, &str)] = &[
    ("vmware-workstation.desktop", "/usr/bin/vmware"),
    ("vmware-netcfg.desktop", "/usr/bin/vmware-netcfg"),
    ("vmware-player.desktop", "/usr/bin/vmplayer"),
];

/// StartupWMClass for .desktop files — without it the taskbar does not
/// associate VMware windows with the launcher (separate, generic icon).
const DESKTOP_WMCLASS: &[(&str, &str)] = &[
    ("vmware-workstation.desktop", "vmware"),
    ("vmware-netcfg.desktop", "vmware-netcfg"),
];

pub fn assemble(
    tx: &Sender<BuildEvent>,
    cancel: &Arc<AtomicBool>,
    cfg: &BuildConfig,
    extracted: &Path,
    staging: &Path,
    built: &BuiltModules,
) -> Result<()> {
    fs::create_dir_all(staging)?;
    let mut mapped_any = false;

    for entry in fs::read_dir(extracted)?.flatten() {
        if cancel.load(Ordering::Relaxed) {
            bail!("Build cancelled by the user");
        }
        let component = entry.file_name().to_string_lossy().into_owned();
        let comp_path = entry.path();
        if !comp_path.is_dir() {
            continue;
        }

        if SKIPPED_COMPONENTS.contains(&component.as_str())
            || component.starts_with("vmware-vix-lib")
        {
            util::log(tx, format!("Skipping component {component} (not needed in the image)"));
            continue;
        }

        // ISO images of the guest OS tools.
        if component.starts_with("vmware-tools-") {
            let iso_dir = staging.join("usr/lib/vmware/isoimages");
            for file in fs::read_dir(&comp_path)?.flatten() {
                let name = file.file_name().to_string_lossy().into_owned();
                if name.ends_with(".iso") || name.ends_with(".iso.sig") {
                    fs::create_dir_all(&iso_dir)?;
                    util::copy_tree(&file.path(), &iso_dir.join(&name))?;
                    mapped_any = true;
                }
            }
            continue;
        }

        for (name, sub, dest) in MAP_RULES {
            if component == *name {
                let src = comp_path.join(sub);
                if !src.exists() {
                    continue;
                }
                util::log(tx, format!("{component}/{sub} → /{dest}"));
                let dest_dir = staging.join(dest);
                if src.is_dir() {
                    util::copy_tree(&src, &dest_dir)?;
                } else {
                    fs::create_dir_all(&dest_dir)?;
                    util::copy_tree(&src, &dest_dir.join(src.file_name().unwrap()))?;
                }
                mapped_any = true;
            }
        }
    }

    if !mapped_any {
        bail!("Failed to map any component — unexpected package layout");
    }

    // Special files from the vmware-vmx component.
    let vmx = extracted.join("vmware-vmx");
    let modules_xml = vmx.join("extra/modules.xml");
    if modules_xml.is_file() {
        util::copy_tree(
            &modules_xml,
            &staging.join("usr/lib/vmware/modules/modules.xml"),
        )?;
    }
    let mut fuse_conf = false;
    let fuse_src = vmx.join("etc/modprobe.d/modprobe-vmware-fuse.conf");
    if fuse_src.is_file() {
        util::copy_tree(
            &fuse_src,
            &staging.join("usr/share/vmware-sysext/etc/modprobe.d/vmware-fuse.conf"),
        )?;
        fuse_conf = true;
    }

    // Do not ship the module source archives — the modules are already compiled,
    // and the presence of sources would tempt vmware-modconfig to rebuild them
    // on a read-only /usr.
    let module_sources = staging.join("usr/lib/vmware/modules/source");
    if module_sources.exists() {
        fs::remove_dir_all(&module_sources)?;
        util::log(tx, "Removed /usr/lib/vmware/modules/source (unneeded source archives)");
    }

    ensure_exec_bits(&staging.join("usr/bin"))?;
    ensure_exec_bits(&staging.join("usr/lib/vmware/bin"))?;
    ensure_exec_bits(&staging.join("usr/lib/vmware/setup"))?;
    // The privilege-elevation helper (vmware-gksu) is sometimes unpacked without the exec bit.
    let gksu_helper = staging.join("usr/lib/vmware/lib/libvmware-gksu.so/gksu-run-helper");
    if gksu_helper.is_file() {
        fs::set_permissions(&gksu_helper, fs::Permissions::from_mode(0o755))?;
    }

    for rel in SUID_BINARIES {
        let path = staging.join(rel);
        if path.is_file() {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o4755))?;
            util::log(tx, format!("setuid: /{rel}"));
        }
    }

    create_symlinks(tx, staging)?;
    substitute_placeholders(tx, staging)?;

    // The compiled kernel modules.
    let misc = staging
        .join("usr/lib/modules")
        .join(&cfg.kernel)
        .join("misc");
    fs::create_dir_all(&misc)?;
    for module in [&built.vmmon, &built.vmnet] {
        let name = module
            .file_name()
            .context("Module path has no file name")?;
        let dest = misc.join(name);
        fs::copy(module, &dest)?;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o644))?;
    }
    util::log(
        tx,
        format!("Modules placed in /usr/lib/modules/{}/misc", cfg.kernel),
    );

    write_own_files(tx, staging, fuse_conf)?;
    write_manifest(tx, cfg, staging, built)?;
    Ok(())
}

/// Reproducibility manifest (methodology from SYSEXT-BUILD-GUIDE.md, item 7):
/// exact versions and the origin of all ingredients in one place — "reproduce
/// the image" should mean executing a recipe, not reconstructing a session
/// from memory.
fn write_manifest(
    tx: &Sender<BuildEvent>,
    cfg: &BuildConfig,
    staging: &Path,
    built: &BuiltModules,
) -> Result<()> {
    let bundle_name = cfg
        .bundle
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let version =
        util::guess_bundle_version(&bundle_name).unwrap_or_else(|| "unknown".to_string());
    util::log(tx, "Computing the SHA-256 of the .bundle package (manifest)…");
    let sha = util::capture_cmd("sha256sum", [cfg.bundle.as_os_str()])
        .ok()
        .and_then(|out| out.split_whitespace().next().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string());
    let date = util::capture_cmd("date", ["-u", "+%Y-%m-%dT%H:%M:%SZ"]).unwrap_or_default();
    let manifest = format!(
        "# vmware.raw image manifest — data to reproduce an identical build\n\
         bundle={bundle_name}\n\
         bundle_path={bundle_path}\n\
         bundle_sha256={sha}\n\
         vmware_version={version}\n\
         kernel={kernel}\n\
         os={os}\n\
         modules_source={source}\n\
         built_utc={date}\n\
         builder=vmware-sysext-builder {builder_version}\n",
        bundle_path = cfg.bundle.display(),
        kernel = cfg.kernel,
        os = super::installed::os_label().unwrap_or_else(|| "unknown".to_string()),
        source = built.source_desc,
        builder_version = env!("CARGO_PKG_VERSION"),
    );
    util::write_file(
        &staging.join("usr/share/vmware-sysext/manifest"),
        &manifest,
        0o644,
    )?;
    Ok(())
}

/// Pre-packing verification ("verify with tools, not from memory"):
/// image structure, dangling symlinks (equivalent of `find . -xtype l`),
/// module vermagic matching the target kernel.
pub fn verify(tx: &Sender<BuildEvent>, staging: &Path, kernel: &str) -> Result<()> {
    // 1. A sysext covers only /usr — nothing else at the top of the tree.
    for entry in fs::read_dir(staging)?.flatten() {
        if entry.file_name() != "usr" {
            bail!(
                "Unexpected top-level entry in the image tree: {} — \
                 a sysext may contain only usr/",
                entry.path().display()
            );
        }
    }

    // 2. Without extension-release the merge would silently skip the image.
    let release = staging.join("usr/lib/extension-release.d/extension-release.vmware");
    if !release.is_file() {
        bail!("Missing {} — the image would not be merged", release.display());
    }

    // 3. Dangling symlinks. Absolute targets outside /usr (e.g. /etc/vmware/…)
    //    may come into existence only at runtime — informational only.
    let mut dangling: Vec<(PathBuf, PathBuf)> = Vec::new();
    scan_symlinks(staging, staging, &mut dangling)?;
    let mut serious = 0usize;
    for (link, target) in &dangling {
        let inside_usr = !target.is_absolute() || target.starts_with("/usr");
        if inside_usr {
            serious += 1;
        }
        util::log(
            tx,
            format!(
                "Dangling symlink: {} → {}{}",
                link.strip_prefix(staging).unwrap_or(link).display(),
                target.display(),
                if inside_usr {
                    ""
                } else {
                    "  (target outside /usr — may come into existence at runtime)"
                }
            ),
        );
    }
    if serious > 0 {
        let _ = tx.send(BuildEvent::Warning(format!(
            "Dangling symlinks detected inside /usr: {serious} — see the log for details"
        )));
    }

    // 4. Module vermagic must start with the target kernel version.
    for name in ["vmmon.ko", "vmnet.ko"] {
        let ko = staging
            .join("usr/lib/modules")
            .join(kernel)
            .join("misc")
            .join(name);
        let vermagic = util::capture_cmd(
            "modinfo",
            [OsStr::new("-F"), OsStr::new("vermagic"), ko.as_os_str()],
        )
        .with_context(|| format!("Failed to read vermagic from {name}"))?;
        let built_for = vermagic.split_whitespace().next().unwrap_or("");
        if built_for != kernel {
            bail!(
                "Module {name} is built for kernel \"{built_for}\" while the image \
                 targets \"{kernel}\" — vermagic mismatch"
            );
        }
        util::log(tx, format!("vermagic {name}: {vermagic} ✓"));
    }

    util::log(
        tx,
        format!(
            "Verification finished: structure OK, vermagic OK, dangling symlinks: {}",
            dangling.len()
        ),
    );
    Ok(())
}

/// Lexical path normalization (folds `..` and `.` without touching the disk).
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Collects symlinks whose target exists neither in the image tree
/// nor (for absolute targets) on the base system.
fn scan_symlinks(
    root: &Path,
    dir: &Path,
    dangling: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<()> {
    for entry in fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        let file_type = fs::symlink_metadata(&path)?.file_type();
        if file_type.is_dir() {
            scan_symlinks(root, &path, dangling)?;
        } else if file_type.is_symlink() {
            let target = fs::read_link(&path)?;
            let in_staging = if target.is_absolute() {
                root.join(target.strip_prefix("/").unwrap_or(&target))
            } else {
                path.parent().unwrap_or(root).join(&target)
            };
            let in_staging = normalize_lexical(&in_staging);
            let ok_in_staging = fs::symlink_metadata(&in_staging).is_ok();
            let ok_on_host = target.is_absolute() && fs::symlink_metadata(&target).is_ok();
            if !ok_in_staging && !ok_on_host {
                dangling.push((path, target));
            }
        }
    }
    Ok(())
}

/// Regular files in program directories must be executable.
fn ensure_exec_bits(dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)?.flatten() {
        let meta = fs::symlink_metadata(entry.path())?;
        if meta.is_file() {
            let mode = meta.permissions().mode();
            if mode & 0o111 != 0o111 {
                fs::set_permissions(
                    entry.path(),
                    fs::Permissions::from_mode((mode & 0o7000) | 0o755),
                )?;
            }
        }
    }
    Ok(())
}

fn create_symlinks(tx: &Sender<BuildEvent>, staging: &Path) -> Result<()> {
    let vmware_bin = staging.join("usr/lib/vmware/bin");
    if vmware_bin.join("appLoader").is_file() {
        for name in APPLOADER_LINKS {
            let link = vmware_bin.join(name);
            if !link.exists() && fs::symlink_metadata(&link).is_err() {
                symlink("appLoader", &link)?;
            }
        }
        util::log(tx, "Created appLoader symlinks in /usr/lib/vmware/bin");
    }
    let usr_bin = staging.join("usr/bin");
    fs::create_dir_all(&usr_bin)?;
    for (name, target) in USR_BIN_LINKS {
        let link = usr_bin.join(name);
        if !link.exists() && fs::symlink_metadata(&link).is_err() {
            symlink(target, &link)?;
        }
    }
    Ok(())
}

fn substitute_placeholders(tx: &Sender<BuildEvent>, staging: &Path) -> Result<()> {
    // @@BINARY@@ in .desktop files
    let apps = staging.join("usr/share/applications");
    if apps.is_dir() {
        for entry in fs::read_dir(&apps)?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".desktop") {
                continue;
            }
            if let Some((_, binary)) = DESKTOP_BINARIES.iter().find(|(n, _)| *n == name) {
                if util::replace_in_file(&entry.path(), "@@BINARY@@", binary)? {
                    util::log(tx, format!("Filled in @@BINARY@@ in {name}"));
                }
            }
            if let Some((_, wmclass)) = DESKTOP_WMCLASS.iter().find(|(n, _)| *n == name) {
                if ensure_startup_wmclass(&entry.path(), wmclass)? {
                    util::log(tx, format!("Added StartupWMClass={wmclass} in {name}"));
                }
            }
        }
    }

    // @@LIBCONF_DIR@@ in the GTK configuration bundled with VMware
    let libconf = staging.join("usr/lib/vmware/libconf");
    if libconf.is_dir() {
        let replaced = replace_recursive(&libconf, "@@LIBCONF_DIR@@", "/usr/lib/vmware/libconf")?;
        if replaced > 0 {
            util::log(tx, format!("Filled in @@LIBCONF_DIR@@ in {replaced} files"));
        }
    }

    // On every start the launcher runs vmware-modconfig (the module rebuild
    // wizard) — a dead end on a read-only /usr. The modules are already in the
    // image, so disable the check the same way the AUR package does: "if true ||".
    for rel in ["usr/bin/vmware", "usr/bin/vmware-tray"] {
        let path = staging.join(rel);
        if path.is_file() && patch_launcher(&path)? {
            util::log(tx, format!("Disabled the module check in /{rel}"));
        }
    }
    Ok(())
}

/// Equivalent of the AUR sed: replaces the line `if "$BINDIR"/vmware-modconfig --appname=…`
/// with `if true ||` (the condition continues on the next line).
fn patch_launcher(path: &Path) -> Result<bool> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Reading {}", path.display()))?;
    let marker = "vmware-modconfig --appname=";
    if !content.contains(marker) {
        return Ok(false);
    }
    let mut changed = false;
    let patched: Vec<String> = content
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("if ") && trimmed.contains(marker) {
                changed = true;
                let indent = &line[..line.len() - trimmed.len()];
                format!("{indent}if true ||")
            } else {
                line.to_string()
            }
        })
        .collect();
    if changed {
        let mut output = patched.join("\n");
        if content.ends_with('\n') {
            output.push('\n');
        }
        fs::write(path, output)?;
    }
    Ok(changed)
}

/// Adds StartupWMClass= to a .desktop file (after the StartupNotify line,
/// or at the end when it is missing). Returns true on change.
fn ensure_startup_wmclass(path: &Path, wmclass: &str) -> Result<bool> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Reading {}", path.display()))?;
    if content.contains("StartupWMClass=") {
        return Ok(false);
    }
    let mut lines: Vec<String> = Vec::new();
    let mut inserted = false;
    for line in content.lines() {
        lines.push(line.to_string());
        if !inserted && line.starts_with("StartupNotify") {
            lines.push(format!("StartupWMClass={wmclass}"));
            inserted = true;
        }
    }
    if !inserted {
        lines.push(format!("StartupWMClass={wmclass}"));
    }
    let mut output = lines.join("\n");
    if content.ends_with('\n') {
        output.push('\n');
    }
    fs::write(path, output)?;
    Ok(true)
}

fn replace_recursive(dir: &Path, from: &str, to: &str) -> Result<usize> {
    let mut count = 0;
    for entry in fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            count += replace_recursive(&path, from, to)?;
        } else if meta.is_file() && util::replace_in_file(&path, from, to)? {
            count += 1;
        }
    }
    Ok(count)
}

/// The extension's own files: sysext metadata, systemd units, the Upholds=
/// drop-in (service autostart after merge), tmpfiles (seeding /etc/vmware),
/// factory /etc contents.
fn write_own_files(tx: &Sender<BuildEvent>, staging: &Path, fuse_conf: bool) -> Result<()> {
    util::write_file(
        &staging.join("usr/lib/extension-release.d/extension-release.vmware"),
        assets::EXTENSION_RELEASE,
        0o644,
    )?;
    util::write_file(
        &staging.join("usr/lib/systemd/system/vmware-modules.service"),
        assets::UNIT_MODULES,
        0o644,
    )?;
    util::write_file(
        &staging.join("usr/lib/systemd/system/vmware-networks.service"),
        assets::UNIT_NETWORKS,
        0o644,
    )?;
    util::write_file(
        &staging.join("usr/lib/systemd/system/vmware-networks-configuration.service"),
        assets::UNIT_NETWORKS_CONFIGURATION,
        0o644,
    )?;
    util::write_file(
        &staging.join("usr/lib/systemd/system/vmware-usbarbitrator.service"),
        assets::UNIT_USBARBITRATOR,
        0o644,
    )?;
    util::write_file(
        &staging.join("usr/lib/systemd/system/multi-user.target.d/10-vmware-sysext.conf"),
        assets::UPHOLDS_DROPIN,
        0o644,
    )?;

    // tmpfiles: C+ merges missing files and never overwrites existing ones —
    // user configuration in /etc/vmware is safe.
    let mut tmpfiles =
        String::from("C+ /etc/vmware - - - - /usr/share/vmware-sysext/etc/vmware\n");
    if fuse_conf {
        tmpfiles.push_str(
            "C+ /etc/modprobe.d/vmware-fuse.conf - - - - \
             /usr/share/vmware-sysext/etc/modprobe.d/vmware-fuse.conf\n",
        );
    }
    tmpfiles.push_str("L /etc/vmware/icu - - - - /usr/lib/vmware/icu\n");
    util::write_file(
        &staging.join("usr/lib/tmpfiles.d/vmware-sysext.conf"),
        &tmpfiles,
        0o644,
    )?;

    util::write_file(
        &staging.join("usr/share/vmware-sysext/etc/vmware/config"),
        assets::ETC_CONFIG,
        0o644,
    )?;
    util::write_file(
        &staging.join("usr/share/vmware-sysext/etc/vmware/bootstrap"),
        assets::ETC_BOOTSTRAP,
        0o644,
    )?;
    util::log(tx, "Wrote extension metadata, systemd units and the Upholds drop-in");
    Ok(())
}

/// depmod on a "superset": a mirror of the base system's modules (symlinks)
/// into which our vmmon.ko/vmnet.ko are added as real files. The resulting
/// modules.* go into the extension and shadow the base ones after the overlay
/// merge, so modprobe sees both the base modules and ours (Flatcar pattern;
/// an incomplete modules.dep would hide the base modules — Flatcar #1576).
pub fn depmod_superset(
    tx: &Sender<BuildEvent>,
    cancel: &Arc<AtomicBool>,
    staging: &Path,
    work: &Path,
    kernel: &str,
) -> Result<()> {
    let base = PathBuf::from("/usr/lib/modules").join(kernel);
    if !base.is_dir() {
        bail!("Base module directory {} is missing", base.display());
    }
    let dep_root = work.join("depmod");
    let mirror = dep_root.join("lib/modules").join(kernel);
    util::log(tx, "Building a mirror of the base modules to compute dependencies…");
    util::symlink_farm(&base, &mirror)?;

    let misc_src = staging.join("usr/lib/modules").join(kernel).join("misc");
    let misc_dst = mirror.join("misc");
    fs::create_dir_all(&misc_dst)?;
    for entry in fs::read_dir(&misc_src)?.flatten() {
        let dest = misc_dst.join(entry.file_name());
        let _ = fs::remove_file(&dest);
        fs::copy(entry.path(), &dest)?;
    }

    let depmod_args: Vec<OsString> = vec![
        "-b".into(),
        dep_root.as_os_str().to_os_string(),
        kernel.into(),
    ];
    util::run_cmd(tx, cancel, None, &[], "depmod", depmod_args)
        .context("depmod failed")?;

    let out_dir = staging.join("usr/lib/modules").join(kernel);
    let mut copied = Vec::new();
    for entry in fs::read_dir(&mirror)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta = fs::symlink_metadata(entry.path())?;
        // Copy only the files generated by depmod (regular files); unchanged
        // entries remained symlinks to the base and are not needed.
        if name.starts_with("modules.") && meta.is_file() {
            fs::copy(entry.path(), out_dir.join(&name))?;
            copied.push(name);
        }
    }
    if !copied.iter().any(|n| n == "modules.dep") {
        bail!("depmod did not generate modules.dep — unexpected result");
    }
    copied.sort();
    util::log(tx, format!("Generated metadata: {}", copied.join(", ")));
    Ok(())
}

/// Is SELinux running in enforcing mode?
fn selinux_enforcing() -> bool {
    fs::read_to_string("/sys/fs/selinux/enforce")
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

/// Builds the vmware.raw image (erofs; squashfs as a fallback). SELinux labels
/// come from the system file_contexts (stock Bazzite runs in enforcing mode —
/// an unlabeled image would not work there). The image is created under a
/// temporary name and replaces vmware.raw only on success — a failure or a
/// cancellation does not destroy the previous working image.
pub fn make_image(
    tx: &Sender<BuildEvent>,
    cancel: &Arc<AtomicBool>,
    staging: &Path,
    work: &Path,
    output_dir: &Path,
) -> Result<PathBuf> {
    util::normalize_dir_modes(staging)
        .context("Normalizing directory permissions of the tree failed")?;

    let raw = output_dir.join("vmware.raw");
    let tmp = output_dir.join(".vmware.raw.tmp");
    let _ = fs::remove_file(&tmp);

    let result = build_image_at(tx, cancel, staging, work, &tmp);
    if let Err(err) = result {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    fs::rename(&tmp, &raw).context("Failed to replace vmware.raw")?;

    // A copy of the manifest next to the image — the recipe to reproduce the build.
    let manifest = staging.join("usr/share/vmware-sysext/manifest");
    if manifest.is_file() {
        let _ = fs::copy(&manifest, output_dir.join("vmware-sysext-manifest.txt"));
    }

    let size = fs::metadata(&raw)?.len();
    util::log(
        tx,
        format!("Image ready: {} ({})", raw.display(), util::human_size(size)),
    );
    Ok(raw)
}

fn build_image_at(
    tx: &Sender<BuildEvent>,
    cancel: &Arc<AtomicBool>,
    staging: &Path,
    work: &Path,
    image: &Path,
) -> Result<()> {
    if util::cmd_exists("mkfs.erofs") {
        let system_contexts = Path::new("/etc/selinux/targeted/contexts/files/file_contexts");
        let contexts = if system_contexts.is_file() {
            util::log(tx, "SELinux labels: system file_contexts");
            system_contexts.to_path_buf()
        } else {
            util::log(tx, "SELinux labels: built-in fallback set");
            let fallback = work.join("file_contexts");
            fs::write(&fallback, assets::FILE_CONTEXTS_FALLBACK)?;
            fallback
        };
        let mut contexts_arg = OsString::from("--file-contexts=");
        contexts_arg.push(contexts.as_os_str());
        let args: Vec<OsString> = vec![
            "-zlz4hc".into(),
            "--all-root".into(),
            contexts_arg,
            image.as_os_str().to_os_string(),
            staging.as_os_str().to_os_string(),
        ];
        let first_try = util::run_cmd(tx, cancel, None, &[], "mkfs.erofs", args);
        if let Err(err) = first_try {
            if cancel.load(Ordering::Relaxed) {
                return Err(err);
            }
            if selinux_enforcing() {
                return Err(err.context(
                    "mkfs.erofs with SELinux labels failed and the system runs in \
                     enforcing mode — an unlabeled image would not work, so no \
                     fallback image is built",
                ));
            }
            let _ = tx.send(BuildEvent::Warning(
                "Image built WITHOUT SELinux labels — it will only work with SELinux \
                 permissive/disabled (stock Bazzite is enforcing!)"
                    .into(),
            ));
            let _ = fs::remove_file(image);
            let args: Vec<OsString> = vec![
                "-zlz4hc".into(),
                "--all-root".into(),
                image.as_os_str().to_os_string(),
                staging.as_os_str().to_os_string(),
            ];
            util::run_cmd(tx, cancel, None, &[], "mkfs.erofs", args)?;
        }
    } else if util::cmd_exists("mksquashfs") {
        if selinux_enforcing() {
            bail!(
                "mkfs.erofs is missing and the system runs in SELinux enforcing mode — \
                 an unlabeled squashfs image would not work. Install erofs-utils."
            );
        }
        let _ = tx.send(BuildEvent::Warning(
            "squashfs image WITHOUT SELinux labels — it will only work with SELinux \
             permissive/disabled (stock Bazzite is enforcing!)"
                .into(),
        ));
        let args: Vec<OsString> = vec![
            staging.as_os_str().to_os_string(),
            image.as_os_str().to_os_string(),
            "-comp".into(),
            "zstd".into(),
            "-all-root".into(),
            "-noappend".into(),
        ];
        util::run_cmd(tx, cancel, None, &[], "mksquashfs", args)?;
    } else {
        bail!("Neither mkfs.erofs nor mksquashfs found — install erofs-utils or squashfs-tools");
    }
    Ok(())
}
