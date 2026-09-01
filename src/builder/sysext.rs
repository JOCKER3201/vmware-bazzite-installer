// Składanie drzewa rozszerzenia systemd-sysext (/usr), wyliczanie metadanych
// modułów (depmod — nadzbiór bazowych + naszych) i budowa obrazu vmware.raw.
//
// Mapowanie komponentów odwzorowuje sprawdzony ręczny układ instalacji
// z pakietu AUR vmware-workstation (potwierdzony niezależnie w NixOS).

use anyhow::{bail, Context, Result};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::builder::{assets, modules::BuiltModules, BuildConfig, BuildEvent};
use crate::util;

/// Mapowanie: (nazwa komponentu, podkatalog/plik w komponencie, katalog docelowy).
/// Zawartość katalogu źródłowego jest scalana z docelowym; pojedynczy plik
/// trafia do wnętrza katalogu docelowego.
const MAP_RULES: &[(&str, &str, &str)] = &[
    // aplikacja główna
    ("vmware-workstation", "bin", "usr/bin"),
    ("vmware-workstation", "lib", "usr/lib/vmware"),
    ("vmware-workstation", "share", "usr/share"),
    ("vmware-workstation", "man", "usr/share/man"),
    ("vmware-workstation", "doc", "usr/share/doc/vmware-workstation"),
    // dodatkowe aplikacje: 25H2+ „vmware-other-apps”, w 17.6.x „vmware-player-app”
    ("vmware-other-apps", "bin", "usr/bin"),
    ("vmware-other-apps", "lib", "usr/lib/vmware"),
    ("vmware-other-apps", "share", "usr/share"),
    ("vmware-other-apps", "doc", "usr/share/doc/vmware-workstation"),
    ("vmware-player-app", "bin", "usr/bin"),
    ("vmware-player-app", "lib", "usr/lib/vmware"),
    ("vmware-player-app", "share", "usr/share"),
    ("vmware-player-app", "doc", "usr/share/doc/vmware-workstation"),
    ("vmware-player-setup", "vmware-config", "usr/lib/vmware/setup"),
    // silnik maszyn wirtualnych (sbin → /usr/bin: tam mieszka vmware-authd)
    ("vmware-vmx", "bin", "usr/bin"),
    ("vmware-vmx", "sbin", "usr/bin"),
    ("vmware-vmx", "lib", "usr/lib/vmware"),
    ("vmware-vmx", "roms", "usr/lib/vmware/roms"),
    // edytor sieci i arbiter USB (lib edytora scala się z /usr/lib/vmware/lib,
    // nie z /usr/lib/vmware — tak robi AUR i NixOS)
    ("vmware-network-editor", "lib", "usr/lib/vmware/lib"),
    ("vmware-network-editor-ui", "share", "usr/share"),
    ("vmware-usbarbitrator", "bin", "usr/lib/vmware/bin"),
];

/// Komponenty świadomie pomijane w minimalnym obrazie.
const SKIPPED_COMPONENTS: &[&str] = &[
    "vmware-installer",
    "vmware-vix-core",
    "vmware-ovftool",
    "vmware-vprobe",
];

/// Pliki z bitem setuid — dokładnie jak w oficjalnej instalacji VMware.
const SUID_BINARIES: &[&str] = &[
    "usr/bin/vmware-authd",
    "usr/lib/vmware/bin/vmware-vmx",
    "usr/lib/vmware/bin/vmware-vmx-debug",
    "usr/lib/vmware/bin/vmware-vmx-stats",
];

/// Nazwy w /usr/lib/vmware/bin będące dowiązaniami do appLoadera
/// (appLoader rozpoznaje program po argv[0]).
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

/// Dowiązania w /usr/bin.
const USR_BIN_LINKS: &[(&str, &str)] = &[
    ("vmrest", "/usr/lib/vmware/bin/appLoader"),
    ("vmware-fuseUI", "/usr/lib/vmware/bin/vmware-fuseUI"),
    ("vmware-mount", "/usr/lib/vmware/bin/vmware-mount"),
    ("vmware-netcfg", "/usr/lib/vmware/bin/vmware-netcfg"),
    ("vmware-usbarbitrator", "/usr/lib/vmware/bin/vmware-usbarbitrator"),
];

/// Uzupełnienie @@BINARY@@ w plikach .desktop.
const DESKTOP_BINARIES: &[(&str, &str)] = &[
    ("vmware-workstation.desktop", "/usr/bin/vmware"),
    ("vmware-netcfg.desktop", "/usr/bin/vmware-netcfg"),
    ("vmware-player.desktop", "/usr/bin/vmplayer"),
];

/// StartupWMClass dla plików .desktop — bez tego pasek zadań nie łączy
/// okien VMware z aktywatorem (osobna, generyczna ikona).
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
            bail!("Budowanie przerwane przez użytkownika");
        }
        let component = entry.file_name().to_string_lossy().into_owned();
        let comp_path = entry.path();
        if !comp_path.is_dir() {
            continue;
        }

        if SKIPPED_COMPONENTS.contains(&component.as_str())
            || component.starts_with("vmware-vix-lib")
        {
            util::log(tx, format!("Pomijam komponent {component} (zbędny w obrazie)"));
            continue;
        }

        // Obrazy ISO narzędzi dla systemów-gości.
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
        bail!("Nie udało się zmapować żadnego komponentu — nieoczekiwany układ pakietu");
    }

    // Pliki specjalne z komponentu vmware-vmx.
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

    // Nie wozimy archiwów źródeł modułów — moduły są już skompilowane,
    // a obecność źródeł kusiłaby vmware-modconfig do przebudowy na RO /usr.
    let module_sources = staging.join("usr/lib/vmware/modules/source");
    if module_sources.exists() {
        fs::remove_dir_all(&module_sources)?;
        util::log(tx, "Usunięto /usr/lib/vmware/modules/source (zbędne archiwa źródeł)");
    }

    ensure_exec_bits(&staging.join("usr/bin"))?;
    ensure_exec_bits(&staging.join("usr/lib/vmware/bin"))?;
    ensure_exec_bits(&staging.join("usr/lib/vmware/setup"))?;
    // Pomocnik podnoszenia uprawnień (vmware-gksu) bywa rozpakowany bez bitu exec.
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

    // Skompilowane moduły jądra.
    let misc = staging
        .join("usr/lib/modules")
        .join(&cfg.kernel)
        .join("misc");
    fs::create_dir_all(&misc)?;
    for module in [&built.vmmon, &built.vmnet] {
        let name = module
            .file_name()
            .context("Ścieżka modułu bez nazwy pliku")?;
        let dest = misc.join(name);
        fs::copy(module, &dest)?;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o644))?;
    }
    util::log(
        tx,
        format!("Moduły umieszczone w /usr/lib/modules/{}/misc", cfg.kernel),
    );

    write_own_files(tx, staging, fuse_conf)?;
    Ok(())
}

/// Zwykłe pliki w katalogach z programami muszą być wykonywalne.
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
        util::log(tx, "Utworzono dowiązania appLoader w /usr/lib/vmware/bin");
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
    // @@BINARY@@ w plikach .desktop
    let apps = staging.join("usr/share/applications");
    if apps.is_dir() {
        for entry in fs::read_dir(&apps)?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".desktop") {
                continue;
            }
            if let Some((_, binary)) = DESKTOP_BINARIES.iter().find(|(n, _)| *n == name) {
                if util::replace_in_file(&entry.path(), "@@BINARY@@", binary)? {
                    util::log(tx, format!("Uzupełniono @@BINARY@@ w {name}"));
                }
            }
            if let Some((_, wmclass)) = DESKTOP_WMCLASS.iter().find(|(n, _)| *n == name) {
                if ensure_startup_wmclass(&entry.path(), wmclass)? {
                    util::log(tx, format!("Dodano StartupWMClass={wmclass} w {name}"));
                }
            }
        }
    }

    // @@LIBCONF_DIR@@ w konfiguracji GTK dołączonej do VMware
    let libconf = staging.join("usr/lib/vmware/libconf");
    if libconf.is_dir() {
        let replaced = replace_recursive(&libconf, "@@LIBCONF_DIR@@", "/usr/lib/vmware/libconf")?;
        if replaced > 0 {
            util::log(tx, format!("Uzupełniono @@LIBCONF_DIR@@ w {replaced} plikach"));
        }
    }

    // Launcher przy każdym starcie odpala vmware-modconfig (kreator
    // przebudowy modułów) — na RO /usr to ślepa uliczka. Moduły są w obrazie,
    // więc wyłączamy test tak samo, jak robi to pakiet AUR: „if true ||”.
    for rel in ["usr/bin/vmware", "usr/bin/vmware-tray"] {
        let path = staging.join(rel);
        if path.is_file() && patch_launcher(&path)? {
            util::log(tx, format!("Wyłączono test modułów w /{rel}"));
        }
    }
    Ok(())
}

/// Odpowiednik seda z AUR: linię „if "$BINDIR"/vmware-modconfig --appname=…”
/// zastępuje „if true ||” (warunek kontynuuje się w następnej linii).
fn patch_launcher(path: &Path) -> Result<bool> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Odczyt {}", path.display()))?;
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

/// Dodaje StartupWMClass= do pliku .desktop (po linii StartupNotify,
/// a gdy jej nie ma — na końcu). Zwraca true przy zmianie.
fn ensure_startup_wmclass(path: &Path, wmclass: &str) -> Result<bool> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Odczyt {}", path.display()))?;
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

/// Pliki własne rozszerzenia: metadane sysext, jednostki systemd, drop-in
/// Upholds= (autostart usług po scaleniu), tmpfiles (zasiew /etc/vmware),
/// fabryczna zawartość /etc.
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

    // tmpfiles: C+ scala brakujące pliki, nigdy nie nadpisuje istniejących —
    // konfiguracja użytkownika w /etc/vmware jest bezpieczna.
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
    util::log(tx, "Zapisano metadane rozszerzenia, jednostki systemd i drop-in Upholds");
    Ok(())
}

/// depmod na „nadzbiorze”: lustro modułów bazowego systemu (dowiązania),
/// do którego dokładamy nasze vmmon.ko/vmnet.ko jako prawdziwe pliki.
/// Wynikowe modules.* trafiają do rozszerzenia i przesłaniają bazowe po
/// scaleniu overlayem, dzięki czemu modprobe widzi i bazowe, i nasze moduły
/// (wzorzec Flatcara; niekompletny modules.dep ukrywałby moduły bazowe —
/// Flatcar #1576).
pub fn depmod_superset(
    tx: &Sender<BuildEvent>,
    cancel: &Arc<AtomicBool>,
    staging: &Path,
    work: &Path,
    kernel: &str,
) -> Result<()> {
    let base = PathBuf::from("/usr/lib/modules").join(kernel);
    if !base.is_dir() {
        bail!("Brak katalogu modułów bazowych {}", base.display());
    }
    let dep_root = work.join("depmod");
    let mirror = dep_root.join("lib/modules").join(kernel);
    util::log(tx, "Buduję lustro modułów bazowych do wyliczenia zależności…");
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
        .context("depmod nie powiódł się")?;

    let out_dir = staging.join("usr/lib/modules").join(kernel);
    let mut copied = Vec::new();
    for entry in fs::read_dir(&mirror)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta = fs::symlink_metadata(entry.path())?;
        // Kopiujemy tylko pliki wygenerowane przez depmod (zwykłe pliki);
        // niezmienione wpisy pozostały dowiązaniami do bazy i nie są potrzebne.
        if name.starts_with("modules.") && meta.is_file() {
            fs::copy(entry.path(), out_dir.join(&name))?;
            copied.push(name);
        }
    }
    if !copied.iter().any(|n| n == "modules.dep") {
        bail!("depmod nie wygenerował modules.dep — nieoczekiwany wynik");
    }
    copied.sort();
    util::log(tx, format!("Wygenerowane metadane: {}", copied.join(", ")));
    Ok(())
}

/// Czy SELinux działa w trybie enforcing?
fn selinux_enforcing() -> bool {
    fs::read_to_string("/sys/fs/selinux/enforce")
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

/// Buduje obraz vmware.raw (erofs; awaryjnie squashfs). Etykiety SELinux
/// pochodzą z systemowego file_contexts (stockowy Bazzite jest w trybie
/// enforcing — obraz bez etykiet by tam nie działał). Obraz powstaje pod
/// nazwą tymczasową i dopiero po sukcesie podmienia vmware.raw — porażka
/// lub anulowanie nie niszczy poprzedniego działającego obrazu.
pub fn make_image(
    tx: &Sender<BuildEvent>,
    cancel: &Arc<AtomicBool>,
    staging: &Path,
    work: &Path,
    output_dir: &Path,
) -> Result<PathBuf> {
    util::normalize_dir_modes(staging)
        .context("Normalizacja uprawnień katalogów drzewa nie powiodła się")?;

    let raw = output_dir.join("vmware.raw");
    let tmp = output_dir.join(".vmware.raw.tmp");
    let _ = fs::remove_file(&tmp);

    let result = build_image_at(tx, cancel, staging, work, &tmp);
    if let Err(err) = result {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    fs::rename(&tmp, &raw).context("Nie udało się podmienić vmware.raw")?;

    let size = fs::metadata(&raw)?.len();
    util::log(
        tx,
        format!("Obraz gotowy: {} ({})", raw.display(), util::human_size(size)),
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
            util::log(tx, "Etykiety SELinux: systemowy file_contexts");
            system_contexts.to_path_buf()
        } else {
            util::log(tx, "Etykiety SELinux: wbudowany zestaw zapasowy");
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
                    "mkfs.erofs z etykietami SELinux nie powiódł się, a system działa \
                     w trybie enforcing — obraz bez etykiet by nie zadziałał, więc nie \
                     buduję wersji zapasowej",
                ));
            }
            let _ = tx.send(BuildEvent::Warning(
                "Obraz zbudowany BEZ etykiet SELinux — zadziała tylko przy SELinux \
                 permissive/wyłączonym (stockowy Bazzite jest enforcing!)"
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
                "Brak mkfs.erofs, a system działa w trybie SELinux enforcing — \
                 obraz squashfs bez etykiet by nie zadziałał. Zainstaluj erofs-utils."
            );
        }
        let _ = tx.send(BuildEvent::Warning(
            "Obraz squashfs BEZ etykiet SELinux — zadziała tylko przy SELinux \
             permissive/wyłączonym (stockowy Bazzite jest enforcing!)"
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
        bail!("Brak mkfs.erofs i mksquashfs — zainstaluj erofs-utils lub squashfs-tools");
    }
    Ok(())
}
