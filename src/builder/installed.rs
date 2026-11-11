// State of the installed extension: reads the manifest of the merged image
// (/usr/share/vmware-sysext/manifest) and compares it with the running system.
// This lets the application detect on its own that the system version changed
// since the build (e.g. a Bazzite F44 → F45 update) and offer a rebuild
// without manually picking a file or a kernel.

use std::fs;
use std::path::{Path, PathBuf};

use crate::util;

/// Manifest of the merged image (key=value, see sysext::write_manifest).
/// Manifests from before 0.2.0 have neither bundle_path nor os.
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

/// Manifest of the currently merged extension — None when the extension
/// is not installed/merged or the manifest is unreadable.
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

/// Human-readable system name from /etc/os-release, e.g. "Bazzite 45".
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

/// Are merged VMware modules present for the given kernel? This decides
/// whether the installed image matches the running system.
pub fn modules_present(kernel: &str) -> bool {
    Path::new("/usr/lib/modules")
        .join(kernel)
        .join("misc/vmmon.ko")
        .is_file()
}

/// Locates the .bundle package used for the previous build: first the full
/// path from the manifest, then a file with the same name in the download
/// directory (xdg-user-dir DOWNLOAD) and in typical locations.
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
        // "Pobrane" is the localized (Polish) downloads directory name.
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
    fn parses_new_format_manifest() {
        let dir = std::env::temp_dir().join("vmware-sysext-test-manifest-new");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manifest");
        fs::write(
            &path,
            "# vmware.raw image manifest — data to reproduce an identical build\n\
             bundle=VMware-Workstation-Full-25H2-24995812.x86_64.bundle\n\
             bundle_path=/home/test/Downloads/VMware-Workstation-Full-25H2-24995812.x86_64.bundle\n\
             bundle_sha256=abc\n\
             vmware_version=25H2\n\
             kernel=7.2.11-ogc4.1.fc45.x86_64\n\
             os=Bazzite 45\n\
             modules_source=vmmon.tar/vmnet.tar archives from the .bundle package\n\
             built_utc=2026-10-28T00:00:00Z\n\
             builder=vmware-sysext-builder 0.2.0\n",
        )
        .unwrap();
        let image = read_from(&path).expect("manifest should parse");
        assert_eq!(image.kernel, "7.2.11-ogc4.1.fc45.x86_64");
        assert_eq!(
            image.bundle_path.as_deref(),
            Some(Path::new(
                "/home/test/Downloads/VMware-Workstation-Full-25H2-24995812.x86_64.bundle"
            ))
        );
        assert_eq!(image.os.as_deref(), Some("Bazzite 45"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_old_format_manifest_without_path_and_os() {
        let dir = std::env::temp_dir().join("vmware-sysext-test-manifest-old");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manifest");
        fs::write(
            &path,
            "# vmware.raw image manifest\n\
             bundle=VMware-Workstation-Full-25H2-24995812.x86_64.bundle\n\
             bundle_sha256=abc\n\
             vmware_version=25H2\n\
             kernel=7.2.1-ogc4.1.fc44.x86_64\n\
             modules_source=vmmon.tar/vmnet.tar archives from the .bundle package\n\
             built_utc=2026-09-05T10:52:18Z\n\
             builder=vmware-sysext-builder 0.1.0\n",
        )
        .unwrap();
        let image = read_from(&path).expect("old manifest should parse too");
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
    fn manifest_without_kernel_is_rejected() {
        let dir = std::env::temp_dir().join("vmware-sysext-test-manifest-empty");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("manifest");
        fs::write(&path, "bundle=x.bundle\n").unwrap();
        assert!(read_from(&path).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Verification on a live system (requires the extension to be installed):
    /// `cargo test -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn autodetection_on_live_system() {
        let image = read().expect("no merged manifest in /usr");
        println!("manifest: {image:?}");
        let running = util::capture_cmd("uname", ["-r"]).unwrap();
        println!("running kernel: {running}");
        println!("modules for the running kernel: {}", modules_present(&running));
        println!("bundle located: {:?}", locate_bundle(&image));
        println!("os: {:?}", os_label());
    }
}
