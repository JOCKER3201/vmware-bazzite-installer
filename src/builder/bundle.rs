// Obsługa pakietu instalacyjnego VMware (.bundle) — samorozpakowującego się
// archiwum. Ekstrakcja nie instaluje niczego w systemie.

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
        bail!("Plik {} nie istnieje", bundle.display());
    }
    // Ekstraktor VMware wymaga, by katalog docelowy NIE istniał
    // („Directory already exists”) — tworzymy tylko katalog nadrzędny.
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
    .context("Ekstrakcja pakietu .bundle nie powiodła się")?;

    let mut components: Vec<String> = fs::read_dir(dest)?
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    components.sort();
    if !components.iter().any(|c| c.starts_with("vmware-")) {
        bail!(
            "Rozpakowany pakiet nie zawiera komponentów vmware-* — \
             czy to na pewno pełny instalator VMware Workstation dla Linuksa (.bundle)?"
        );
    }
    util::log(tx, format!("Znalezione komponenty: {}", components.join(", ")));
    Ok(dest.to_path_buf())
}

/// Szuka archiwów źródeł modułów jądra w rozpakowanym pakiecie.
pub fn find_module_tars(extracted: &Path) -> Option<(PathBuf, PathBuf)> {
    let vmmon = util::find_file(extracted, "vmmon.tar")?;
    let vmnet = util::find_file(extracted, "vmnet.tar")?;
    Some((vmmon, vmnet))
}
