// Potok budowania: .bundle → ekstrakcja → moduły jądra → drzewo /usr → depmod → vmware.raw
// Całość działa w osobnym wątku i raportuje postęp kanałem zdarzeń do GUI.

pub mod assets;
pub mod bundle;
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
    /// Ostrzeżenie do pokazania w panelu wyniku (nie tylko w dzienniku).
    Warning(String),
    /// Rozpoczęcie etapu o podanym indeksie (patrz STAGES).
    Stage(usize),
    Done(Result<PathBuf, String>),
}

pub const STAGES: &[&str] = &[
    "Przygotowanie katalogu roboczego",
    "Ekstrakcja pakietu .bundle",
    "Kompilacja modułów jądra (vmmon, vmnet)",
    "Składanie drzewa rozszerzenia (/usr)",
    "Metadane modułów (depmod)",
    "Budowanie obrazu vmware.raw",
];

#[derive(Clone)]
pub struct BuildConfig {
    pub bundle: PathBuf,
    /// Wersja jądra, dla której kompilujemy moduły (wynik `uname -r`).
    pub kernel: String,
    /// Opcjonalny katalog z łatanymi źródłami modułów
    /// (vmmon-only/ i vmnet-only/ albo vmmon.tar i vmnet.tar).
    pub custom_sources: Option<PathBuf>,
    /// Opcjonalne podpisywanie modułów pod Secure Boot (klucz prywatny + certyfikat).
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
        bail!("Budowanie przerwane przez użytkownika");
    }
    Ok(())
}

fn run(cfg: &BuildConfig, tx: &Sender<BuildEvent>, cancel: &Arc<AtomicBool>) -> Result<PathBuf> {
    let work = cfg.output_dir.join("vmware-sysext-work");
    let marker = work.join(".vmware-sysext-builder");

    stage(tx, 0);
    if work.exists() {
        // Bezpiecznik: usuwamy tylko katalog, który sami założyliśmy.
        if !marker.exists() {
            bail!(
                "Katalog {} istnieje, ale nie wygląda na katalog roboczy tego programu \
                 (brak pliku znacznika). Nie usuwam go — wybierz inny katalog wyjściowy \
                 albo usuń go ręcznie.",
                work.display()
            );
        }
        util::log(tx, format!("Czyszczę poprzedni katalog roboczy: {}", work.display()));
        fs::remove_dir_all(&work).context("Nie udało się wyczyścić katalogu roboczego")?;
    }
    fs::create_dir_all(&work).context("Nie udało się utworzyć katalogu roboczego")?;
    fs::write(&marker, "katalog roboczy vmware-sysext-builder\n")?;

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
    let raw = sysext::make_image(tx, cancel, &staging, &work, &cfg.output_dir)?;

    util::log(tx, format!("Gotowe: {}", raw.display()));
    Ok(raw)
}

/// Polecenia do ręcznej instalacji obrazu (pokazywane użytkownikowi).
/// Kolejność ma znaczenie: refresh z --no-reload scala obraz bez
/// przeładowania menedżera usług, potem tmpfiles zasiewa /etc/vmware,
/// a dopiero daemon-reload uruchamia usługi przez drop-in Upholds= —
/// inaczej usługi wystartowałyby przed powstaniem /etc/vmware.
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
        "echo 'Instalacja zakończona.'\n",
    )
    .to_string()
}

/// Instalacja obrazu przez pkexec — uruchamiana wyłącznie na życzenie użytkownika.
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
