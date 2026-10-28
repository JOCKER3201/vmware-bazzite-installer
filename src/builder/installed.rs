// Stan zainstalowanego rozszerzenia: odczyt manifestu scalonego obrazu
// (/usr/share/vmware-sysext/manifest) i porównanie z uruchomionym systemem.
// Dzięki temu aplikacja sama wykrywa, że system zmienił wersję od czasu
// builda (np. aktualizacja Bazzite F44 → F45 z nowym jądrem) i potrafi
// zaproponować przebudowę bez ręcznego wybierania pliku czy jądra.

use std::fs;
use std::path::{Path, PathBuf};

use crate::util;

/// Manifest scalonego obrazu (klucz=wartość, patrz sysext::write_manifest).
/// Manifesty sprzed wersji 0.2.0 nie mają bundle_path ani os.
#[derive(Clone, Debug, Default)]
pub struct InstalledImage {
    pub bundle_name: String,
    pub bundle_path: Option<PathBuf>,
    pub vmware_version: Option<String>,
    pub kernel: String,
    pub built_utc: Option<String>,
    pub os: Option<String>,
}

pub const MANIFEST_PATH: &str = "/usr/share/vmware-sysext/manifest";

/// Manifest aktualnie scalonego rozszerzenia — None, gdy rozszerzenie
/// nie jest zainstalowane/scalone albo manifest jest nieczytelny.
pub fn read() -> Option<InstalledImage> {
    read_from(Path::new(MANIFEST_PATH))
}

fn read_from(path: &Path) -> Option<InstalledImage> {
    let text = fs::read_to_string(path).ok()?;
    let mut image = InstalledImage::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "bundle" => image.bundle_name = value.to_string(),
            "bundle_path" => image.bundle_path = Some(PathBuf::from(value)),
            "vmware_version" => image.vmware_version = Some(value.to_string()),
            "kernel" => image.kernel = value.to_string(),
            "built_utc" => image.built_utc = Some(value.to_string()),
            "os" => image.os = Some(value.to_string()),
            _ => {}
        }
    }
    if image.kernel.is_empty() {
        None
    } else {
        Some(image)
    }
}

/// Czytelna nazwa systemu z /etc/os-release, np. „Bazzite 45”.
pub fn os_label() -> Option<String> {
    let text = fs::read_to_string("/etc/os-release").ok()?;
    let mut name = None;
    let mut version = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("NAME=") {
            name = Some(unquote(v));
        } else if let Some(v) = line.strip_prefix("VERSION_ID=") {
            version = Some(unquote(v));
        }
    }
    match (name, version) {
        (Some(n), Some(v)) => Some(format!("{n} {v}")),
        (Some(n), None) => Some(n),
        _ => None,
    }
}

fn unquote(value: &str) -> String {
    value.trim().trim_matches('"').to_string()
}

/// Czy dla podanego jądra są scalone moduły VMware? To rozstrzyga, czy
/// zainstalowany obraz pasuje do uruchomionego systemu.
pub fn modules_present(kernel: &str) -> bool {
    Path::new("/usr/lib/modules")
        .join(kernel)
        .join("misc/vmmon.ko")
        .is_file()
}

/// Odnajduje pakiet .bundle użyty do poprzedniego builda: najpierw pełna
/// ścieżka z manifestu, potem plik o tej samej nazwie w katalogu pobrań
/// (xdg-user-dir DOWNLOAD) i typowych lokalizacjach.
pub fn locate_bundle(image: &InstalledImage) -> Option<PathBuf> {
    if let Some(path) = &image.bundle_path {
        if path.is_file() {
            return Some(path.clone());
        }
    }
    if image.bundle_name.is_empty() {
        return None;
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(out) = util::capture_cmd("xdg-user-dir", ["DOWNLOAD"]) {
        if !out.is_empty() {
            dirs.push(PathBuf::from(out));
        }
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join("Pobrane"));
        dirs.push(home.join("Downloads"));
        dirs.push(home);
    }
    dirs.dedup();
    for dir in dirs {
        let candidate = dir.join(&image.bundle_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsuje_manifest_nowego_formatu() {
        let dir = std::env::temp_dir().join("vmware-sysext-test-manifest-nowy");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manifest");
        fs::write(
            &path,
            "# Manifest obrazu vmware.raw — dane do odtworzenia identycznego builda\n\
             bundle=VMware-Workstation-Full-25H2-24995812.x86_64.bundle\n\
             bundle_path=/home/test/Pobrane/VMware-Workstation-Full-25H2-24995812.x86_64.bundle\n\
             bundle_sha256=abc\n\
             vmware_version=25H2\n\
             kernel=7.2.11-ogc4.1.fc45.x86_64\n\
             os=Bazzite 45\n\
             modules_source=archiwa vmmon.tar/vmnet.tar z pakietu .bundle\n\
             built_utc=2026-10-28T00:00:00Z\n\
             builder=vmware-sysext-builder 0.2.0\n",
        )
        .unwrap();
        let image = read_from(&path).expect("manifest powinien się sparsować");
        assert_eq!(image.kernel, "7.2.11-ogc4.1.fc45.x86_64");
        assert_eq!(
            image.bundle_path.as_deref(),
            Some(Path::new(
                "/home/test/Pobrane/VMware-Workstation-Full-25H2-24995812.x86_64.bundle"
            ))
        );
        assert_eq!(image.os.as_deref(), Some("Bazzite 45"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parsuje_manifest_starego_formatu_bez_sciezki_i_os() {
        let dir = std::env::temp_dir().join("vmware-sysext-test-manifest-stary");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manifest");
        fs::write(
            &path,
            "# Manifest obrazu vmware.raw\n\
             bundle=VMware-Workstation-Full-25H2-24995812.x86_64.bundle\n\
             bundle_sha256=abc\n\
             vmware_version=25H2\n\
             kernel=7.2.1-ogc4.1.fc44.x86_64\n\
             modules_source=archiwa vmmon.tar/vmnet.tar z pakietu .bundle\n\
             built_utc=2026-09-05T10:52:18Z\n\
             builder=vmware-sysext-builder 0.1.0\n",
        )
        .unwrap();
        let image = read_from(&path).expect("stary manifest też powinien się sparsować");
        assert_eq!(image.kernel, "7.2.1-ogc4.1.fc44.x86_64");
        assert_eq!(image.bundle_path, None);
        assert_eq!(image.os, None);
        assert_eq!(
            image.bundle_name,
            "VMware-Workstation-Full-25H2-24995812.x86_64.bundle"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_bez_jadra_jest_odrzucany() {
        let dir = std::env::temp_dir().join("vmware-sysext-test-manifest-pusty");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manifest");
        fs::write(&path, "bundle=x.bundle\n").unwrap();
        assert!(read_from(&path).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Weryfikacja na żywym systemie (wymaga zainstalowanego rozszerzenia):
    /// `cargo test -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn autodetekcja_na_zywym_systemie() {
        let image = read().expect("brak scalonego manifestu w /usr");
        println!("manifest: {image:?}");
        let running = util::capture_cmd("uname", ["-r"]).unwrap();
        println!("uruchomione jądro: {running}");
        println!("moduły dla uruchomionego jądra: {}", modules_present(&running));
        println!("odnaleziony bundle: {:?}", locate_bundle(&image));
        println!("system: {:?}", os_label());
    }
}
