// Build pipeline: .bundle → extraction → kernel modules → /usr tree → depmod → vmware.raw
// Everything runs on a separate thread and reports progress to the GUI over an event channel.

pub mod assets;
pub mod bundle;
pub mod installed;
pub mod kernel;
pub mod modules;
pub mod sysext;

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crate::util;

#[derive(Debug)]
pub enum BuildEvent {
    Log(String),
    /// Warning to be shown in the result panel (not only in the log).
    Warning(String),
    /// Start of the stage with the given index (see STAGES).
    Stage(usize),
    Done(Result<PathBuf, String>),
}

pub const STAGES: &[&str] = &[
    "Preparing the working directory",
    "Extracting the .bundle package",
    "Compiling kernel modules (vmmon, vmnet)",
    "Assembling the extension tree (/usr)",
    "Module metadata (depmod)",
    "Verifying the tree (structure, symlinks, vermagic)",
    "Building the vmware.raw image",
];

#[derive(Clone)]
pub struct BuildConfig {
    pub bundle: PathBuf,
    /// Kernel version the modules are compiled for (output of `uname -r`).
    pub kernel: String,
    /// Optional directory with patched module sources
    /// (vmmon-only/ and vmnet-only/, or vmmon.tar and vmnet.tar).
    pub custom_sources: Option<PathBuf>,
    /// Optional module signing for Secure Boot (private key + certificate).
    pub sign_key: Option<PathBuf>,
    pub sign_cert: Option<PathBuf>,
    pub output_dir: PathBuf,
}

pub struct BuildTask {
    pub rx: Receiver<BuildEvent>,
    pub cancel: Arc<AtomicBool>,
    pub handle: Option<JoinHandle<()>>,
}

pub fn spawn_build(cfg: BuildConfig) -> BuildTask {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_flag = cancel.clone();
    let handle = thread::spawn(move || {
        let result = run(&cfg, &tx, &cancel_flag).map_err(|e| format!("{e:#}"));
        let _ = tx.send(BuildEvent::Done(result));
    });
    BuildTask {
        rx,
        cancel,
        handle: Some(handle),
    }
}

fn stage(tx: &Sender<BuildEvent>, index: usize) {
    let _ = tx.send(BuildEvent::Stage(index));
    util::log(
        tx,
        format!("──── [{}/{}] {} ────", index + 1, STAGES.len(), STAGES[index]),
    );
}

fn check_cancel(cancel: &Arc<AtomicBool>) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("Build cancelled by the user");
    }
    Ok(())
}

fn run(cfg: &BuildConfig, tx: &Sender<BuildEvent>, cancel: &Arc<AtomicBool>) -> Result<PathBuf> {
    let work = cfg.output_dir.join("vmware-sysext-work");
    let marker = work.join(".vmware-sysext-builder");

    stage(tx, 0);
    if work.exists() {
        // Safety check: only remove a directory we created ourselves.
        if !marker.exists() {
            bail!(
                "Directory {} exists but does not look like this program's working \
                 directory (marker file missing). Refusing to remove it — choose a \
                 different output directory or delete it manually.",
                work.display()
            );
        }
        util::log(tx, format!("Cleaning up the previous working directory: {}", work.display()));
        fs::remove_dir_all(&work).context("Failed to clean the working directory")?;
    }
    fs::create_dir_all(&work).context("Failed to create the working directory")?;
    fs::write(&marker, "vmware-sysext-builder working directory\n")?;

    stage(tx, 1);
    let extracted = bundle::extract(tx, cancel, &cfg.bundle, &work.join("extracted"))?;

    check_cancel(cancel)?;
    stage(tx, 2);
    let built = modules::build_all(tx, cancel, cfg, &extracted, &work)?;

    check_cancel(cancel)?;
    stage(tx, 3);
    let staging = work.join("staging");
    sysext::assemble(tx, cancel, cfg, &extracted, &staging, &built)?;

    check_cancel(cancel)?;
    stage(tx, 4);
    sysext::depmod_superset(tx, cancel, &staging, &work, &cfg.kernel)?;

    check_cancel(cancel)?;
    stage(tx, 5);
    sysext::verify(tx, &staging, &cfg.kernel)?;

    check_cancel(cancel)?;
    stage(tx, 6);
    let raw = sysext::make_image(tx, cancel, &staging, &work, &cfg.output_dir)?;

    util::log(tx, format!("Done: {}", raw.display()));
    Ok(raw)
}

/// Commands for installing the image manually (shown to the user).
/// Order matters: refresh with --no-reload merges the image without
/// reloading the service manager, then tmpfiles seeds /etc/vmware,
/// and only daemon-reload starts the services via the Upholds= drop-in —
/// otherwise the services would start before /etc/vmware exists.
pub fn manual_commands(raw: &Path) -> String {
    format!(
        "sudo install -D -m 0644 '{raw}' /var/lib/extensions/vmware.raw\n\
         sudo restorecon -RF /var/lib/extensions\n\
         sudo systemd-sysext refresh --no-reload\n\
         sudo systemd-tmpfiles --create /usr/lib/tmpfiles.d/vmware-sysext.conf\n\
         sudo systemctl daemon-reload",
        raw = raw.display()
    )
}

fn install_script() -> String {
    concat!(
        "set -e\n",
        "install -D -m 0644 \"$1\" /var/lib/extensions/vmware.raw\n",
        "command -v restorecon >/dev/null && restorecon -RF /var/lib/extensions || true\n",
        "systemd-sysext refresh --no-reload\n",
        "systemd-tmpfiles --create /usr/lib/tmpfiles.d/vmware-sysext.conf\n",
        "systemctl daemon-reload\n",
        "echo 'Installation finished.'\n",
    )
    .to_string()
}

/// Installs the image via pkexec — run only at the user's explicit request.
pub fn spawn_install(raw: PathBuf) -> BuildTask {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_flag = cancel.clone();
    let handle = thread::spawn(move || {
        let script = install_script();
        let mut args: Vec<std::ffi::OsString> =
            vec!["sh".into(), "-c".into(), script.into(), "sh".into()];
        args.push(raw.clone().into_os_string());
        let result = util::run_cmd(&tx, &cancel_flag, None, &[], "pkexec", args)
            .map(|_| raw.clone())
            .map_err(|e| format!("{e:#}"));
        let _ = tx.send(BuildEvent::Done(result));
    });
    BuildTask {
        rx,
        cancel,
        handle: Some(handle),
    }
}
