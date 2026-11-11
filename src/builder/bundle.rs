// Handling of the VMware installer package (.bundle) — a self-extracting
// archive. Extraction does not install anything on the system.

use anyhow::{bail, Context, Result};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::builder::BuildEvent;
use crate::util;

pub fn extract(
    tx: &Sender<BuildEvent>,
    cancel: &Arc<AtomicBool>,
    bundle: &Path,
    dest: &Path,
) -> Result<PathBuf> {
    if !bundle.is_file() {
        bail!("File {} does not exist", bundle.display());
    }
    // The VMware extractor requires the destination directory NOT to exist
    // ("Directory already exists") — create only the parent directory.
    if dest.exists() {
        fs::remove_dir_all(dest)?;
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    util::run_cmd(
        tx,
        cancel,
        None,
        &[],
        "sh",
        [bundle.as_os_str(), OsStr::new("--extract"), dest.as_os_str()],
    )
    .context("Extraction of the .bundle package failed")?;

    let mut components: Vec<String> = fs::read_dir(dest)?
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    components.sort();
    if !components.iter().any(|c| c.starts_with("vmware-")) {
        bail!(
            "The extracted package contains no vmware-* components — \
             is this really a full VMware Workstation installer for Linux (.bundle)?"
        );
    }
    util::log(tx, format!("Components found: {}", components.join(", ")));
    Ok(dest.to_path_buf())
}

/// Looks for the kernel module source archives in the extracted package.
pub fn find_module_tars(extracted: &Path) -> Option<(PathBuf, PathBuf)> {
    let vmmon = util::find_file(extracted, "vmmon.tar")?;
    let vmnet = util::find_file(extracted, "vmnet.tar")?;
    Some((vmmon, vmnet))
}
