// Kompilacja modułów jądra vmmon i vmnet ze źródeł dostarczonych w pakiecie
// VMware (vmmon.tar / vmnet.tar) albo z łatanych źródeł wskazanych przez
// użytkownika (układ forka vmware-host-modules: vmmon-only/ i vmnet-only/).

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
    /// Pochodzenie źródeł modułów — trafia do manifestu odtwarzalności.
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
                "Brak katalogu źródeł modułu {name} — oczekiwano {}",
                dir.display()
            );
        }
        util::log(tx, format!("Kompiluję moduł {name} dla jądra {}", cfg.kernel));
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
                "Kompilacja modułu {name} nie powiodła się. Źródła z pakietu VMware \
                 najpewniej nie obsługują jądra {kernel} — w „Opcjach zaawansowanych” \
                 wskaż katalog z łatanymi źródłami (fork vmware-host-modules, układ \
                 vmmon-only/ i vmnet-only/).",
                kernel = cfg.kernel
            )
        })?;
        let direct = dir.join(format!("{name}.ko"));
        let ko = if direct.is_file() {
            direct
        } else {
            util::find_file(&dir, &format!("{name}.ko")).ok_or_else(|| {
                anyhow!("Nie znaleziono zbudowanego pliku {name}.ko w {}", dir.display())
            })?
        };
        util::log(tx, format!("Zbudowano: {}", ko.display()));
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
        util::log(tx, format!("Katalog użytkownika: {}", custom.display()));
        if custom.join("vmmon-only").is_dir() && custom.join("vmnet-only").is_dir() {
            util::log(tx, "Używam źródeł vmmon-only/ i vmnet-only/ z katalogu użytkownika");
            util::copy_tree(&custom.join("vmmon-only"), &srcroot.join("vmmon-only"))?;
            util::copy_tree(&custom.join("vmnet-only"), &srcroot.join("vmnet-only"))?;
            source_desc = format!("własne źródła (vmmon-only/vmnet-only): {}", custom.display());
            have_sources = true;
        } else if custom.join("vmmon.tar").is_file() && custom.join("vmnet.tar").is_file() {
            util::log(tx, "Używam vmmon.tar i vmnet.tar z katalogu użytkownika");
            untar(&custom.join("vmmon.tar"), srcroot)?;
            untar(&custom.join("vmnet.tar"), srcroot)?;
            source_desc = format!("własne archiwa vmmon.tar/vmnet.tar: {}", custom.display());
            have_sources = true;
        }
    }
    if !have_sources {
        let (vmmon_tar, vmnet_tar) = bundle::find_module_tars(extracted)
            .context("Nie znaleziono vmmon.tar / vmnet.tar w rozpakowanym pakiecie")?;
        util::log(
            tx,
            format!("Źródła modułów: {} oraz {}", vmmon_tar.display(), vmnet_tar.display()),
        );
        untar(&vmmon_tar, srcroot)?;
        untar(&vmnet_tar, srcroot)?;
        source_desc = "archiwa vmmon.tar/vmnet.tar z pakietu .bundle".to_string();
    }

    // Opcjonalne łatki na nowe jądra (układ pakietu AUR vmware-workstation:
    // vmmon.patch i vmnet.patch nakładane przez `patch -p2` wewnątrz *-only).
    if let Some(custom) = &cfg.custom_sources {
        let mut any_patch = false;
        for (module, patch_name) in [("vmmon", "vmmon.patch"), ("vmnet", "vmnet.patch")] {
            let patch_file = custom.join(patch_name);
            if !patch_file.is_file() {
                continue;
            }
            if !util::cmd_exists("patch") {
                bail!("Znaleziono {patch_name}, ale brak programu `patch` w systemie");
            }
            any_patch = true;
            util::log(tx, format!("Nakładam łatkę {patch_name} na {module}-only"));
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
            .with_context(|| format!("Nałożenie łatki {patch_name} nie powiodło się"))?;
        }
        if any_patch {
            source_desc.push_str(&format!(" + łatki z {}", custom.display()));
        }
        if !have_sources && !any_patch {
            bail!(
                "Katalog {} nie zawiera ani źródeł (vmmon-only/ i vmnet-only/ lub \
                 vmmon.tar i vmnet.tar), ani łatek (vmmon.patch / vmnet.patch)",
                custom.display()
            );
        }
    }
    Ok(source_desc)
}

fn untar(tar_path: &Path, dest: &Path) -> Result<()> {
    let file = File::open(tar_path)
        .with_context(|| format!("Otwarcie {}", tar_path.display()))?;
    tar::Archive::new(file)
        .unpack(dest)
        .with_context(|| format!("Rozpakowanie {}", tar_path.display()))?;
    Ok(())
}

/// Podpisywanie modułów pod Secure Boot narzędziem sign-file z drzewa jądra.
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
        bail!("Brak narzędzia sign-file: {}", sign_file.display());
    }
    let program = sign_file.to_string_lossy().into_owned();
    for module in modules {
        util::log(tx, format!("Podpisuję moduł {}", module.display()));
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
