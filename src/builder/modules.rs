// Compilation of the vmmon and vmnet kernel modules from the sources shipped
// in the VMware package (vmmon.tar / vmnet.tar) or from patched sources chosen
// by the user (vmware-host-modules fork layout: vmmon-only/ and vmnet-only/).

use anyhow::{anyhow, bail, Context, Result};
use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::builder::{bundle, BuildConfig, BuildEvent};
use crate::util;

pub struct BuiltModules {
    pub vmmon: PathBuf,
    pub vmnet: PathBuf,
    /// Origin of the module sources — goes into the reproducibility manifest.
    pub source_desc: String,
}

pub fn build_all(
    tx: &Sender<BuildEvent>,
    cancel: &Arc<AtomicBool>,
    cfg: &BuildConfig,
    extracted: &Path,
    work: &Path,
) -> Result<BuiltModules> {
    let srcroot = work.join("modules-src");
    fs::create_dir_all(&srcroot)?;
    let source_desc = prepare_sources(tx, cancel, cfg, extracted, &srcroot)?;

    let jobs = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);

    let mut built: Vec<PathBuf> = Vec::new();
    for name in ["vmmon", "vmnet"] {
        let dir = srcroot.join(format!("{name}-only"));
        if !dir.is_dir() {
            bail!(
                "Missing source directory for module {name} — expected {}",
                dir.display()
            );
        }
        util::log(tx, format!("Compiling module {name} for kernel {}", cfg.kernel));
        util::run_cmd(
            tx,
            cancel,
            Some(&dir),
            &[],
            "make",
            [format!("VM_UNAME={}", cfg.kernel), format!("-j{jobs}")],
        )
        .with_context(|| {
            format!(
                "Compilation of module {name} failed. The sources from the VMware \
                 package most likely do not support kernel {kernel} — under \
                 \"Advanced options\" point to a directory with patched sources \
                 (vmware-host-modules fork, vmmon-only/ and vmnet-only/ layout).",
                kernel = cfg.kernel
            )
        })?;
        let direct = dir.join(format!("{name}.ko"));
        let ko = if direct.is_file() {
            direct
        } else {
            util::find_file(&dir, &format!("{name}.ko")).ok_or_else(|| {
                anyhow!("Built file {name}.ko not found in {}", dir.display())
            })?
        };
        util::log(tx, format!("Built: {}", ko.display()));
        built.push(ko);
    }

    if let (Some(key), Some(cert)) = (&cfg.sign_key, &cfg.sign_cert) {
        sign_modules(tx, cancel, &cfg.kernel, key, cert, &built)?;
    }

    Ok(BuiltModules {
        vmmon: built[0].clone(),
        vmnet: built[1].clone(),
        source_desc,
    })
}

fn prepare_sources(
    tx: &Sender<BuildEvent>,
    cancel: &Arc<AtomicBool>,
    cfg: &BuildConfig,
    extracted: &Path,
    srcroot: &Path,
) -> Result<String> {
    let mut have_sources = false;
    let mut source_desc = String::new();
    if let Some(custom) = &cfg.custom_sources {
        util::log(tx, format!("User directory: {}", custom.display()));
        if custom.join("vmmon-only").is_dir() && custom.join("vmnet-only").is_dir() {
            util::log(tx, "Using vmmon-only/ and vmnet-only/ sources from the user directory");
            util::copy_tree(&custom.join("vmmon-only"), &srcroot.join("vmmon-only"))?;
            util::copy_tree(&custom.join("vmnet-only"), &srcroot.join("vmnet-only"))?;
            source_desc = format!("custom sources (vmmon-only/vmnet-only): {}", custom.display());
            have_sources = true;
        } else if custom.join("vmmon.tar").is_file() && custom.join("vmnet.tar").is_file() {
            util::log(tx, "Using vmmon.tar and vmnet.tar from the user directory");
            untar(&custom.join("vmmon.tar"), srcroot)?;
            untar(&custom.join("vmnet.tar"), srcroot)?;
            source_desc = format!("custom vmmon.tar/vmnet.tar archives: {}", custom.display());
            have_sources = true;
        }
    }
    if !have_sources {
        let (vmmon_tar, vmnet_tar) = bundle::find_module_tars(extracted)
            .context("vmmon.tar / vmnet.tar not found in the extracted package")?;
        util::log(
            tx,
            format!("Module sources: {} and {}", vmmon_tar.display(), vmnet_tar.display()),
        );
        untar(&vmmon_tar, srcroot)?;
        untar(&vmnet_tar, srcroot)?;
        source_desc = "vmmon.tar/vmnet.tar archives from the .bundle package".to_string();
    }

    // Optional patches for new kernels (AUR vmware-workstation package layout:
    // vmmon.patch and vmnet.patch applied with `patch -p2` inside *-only).
    if let Some(custom) = &cfg.custom_sources {
        let mut any_patch = false;
        for (module, patch_name) in [("vmmon", "vmmon.patch"), ("vmnet", "vmnet.patch")] {
            let patch_file = custom.join(patch_name);
            if !patch_file.is_file() {
                continue;
            }
            if !util::cmd_exists("patch") {
                bail!("Found {patch_name}, but the `patch` program is not available on this system");
            }
            any_patch = true;
            util::log(tx, format!("Applying patch {patch_name} to {module}-only"));
            let patch_args: Vec<OsString> = vec![
                "-p2".into(),
                "-N".into(),
                "-i".into(),
                patch_file.as_os_str().to_os_string(),
            ];
            util::run_cmd(
                tx,
                cancel,
                Some(&srcroot.join(format!("{module}-only"))),
                &[],
                "patch",
                patch_args,
            )
            .with_context(|| format!("Applying patch {patch_name} failed"))?;
        }
        if any_patch {
            source_desc.push_str(&format!(" + patches from {}", custom.display()));
        }
        if !have_sources && !any_patch {
            bail!(
                "Directory {} contains neither sources (vmmon-only/ and vmnet-only/, or \
                 vmmon.tar and vmnet.tar) nor patches (vmmon.patch / vmnet.patch)",
                custom.display()
            );
        }
    }
    Ok(source_desc)
}

fn untar(tar_path: &Path, dest: &Path) -> Result<()> {
    let file = File::open(tar_path)
        .with_context(|| format!("Opening {}", tar_path.display()))?;
    tar::Archive::new(file)
        .unpack(dest)
        .with_context(|| format!("Unpacking {}", tar_path.display()))?;
    Ok(())
}

/// Signs modules for Secure Boot using the sign-file tool from the kernel tree.
fn sign_modules(
    tx: &Sender<BuildEvent>,
    cancel: &Arc<AtomicBool>,
    kernel: &str,
    key: &Path,
    cert: &Path,
    modules: &[PathBuf],
) -> Result<()> {
    let sign_file = PathBuf::from("/usr/lib/modules")
        .join(kernel)
        .join("build/scripts/sign-file");
    if !sign_file.is_file() {
        bail!("sign-file tool missing: {}", sign_file.display());
    }
    let program = sign_file.to_string_lossy().into_owned();
    for module in modules {
        util::log(tx, format!("Signing module {}", module.display()));
        let sign_args: Vec<OsString> = vec![
            "sha256".into(),
            key.as_os_str().to_os_string(),
            cert.as_os_str().to_os_string(),
            module.as_os_str().to_os_string(),
        ];
        util::run_cmd(tx, cancel, None, &[], &program, sign_args)?;
    }
    Ok(())
}
