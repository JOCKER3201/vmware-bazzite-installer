// Wykrywanie zainstalowanych wersji jądra i dostępności nagłówków (kernel-devel).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[derive(Clone, Debug)]
pub struct KernelInfo {
    pub version: String,
    /// Czy dostępne są nagłówki do kompilacji modułów (…/build/Makefile).
    pub devel: bool,
    /// Czy to jądro aktualnie uruchomione (uname -r).
    pub running: bool,
}

impl KernelInfo {
    pub fn build_dir(&self) -> PathBuf {
        PathBuf::from("/usr/lib/modules")
            .join(&self.version)
            .join("build")
    }
}

pub fn detect() -> Vec<KernelInfo> {
    let running = Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default();

    let mut kernels = Vec::new();
    if let Ok(dir) = fs::read_dir("/usr/lib/modules") {
        for entry in dir.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let version = entry.file_name().to_string_lossy().into_owned();
            let devel = path.join("build").join("Makefile").is_file();
            // Katalog bez kernel/, vmlinuz i build/ to nie zainstalowane jądro,
            // tylko ślad po scalonym rozszerzeniu (moduły vmmon/vmnet starego
            // jądra z vmware.raw po aktualizacji systemu) — pomijamy.
            if !devel && !path.join("kernel").is_dir() && !path.join("vmlinuz").is_file() {
                continue;
            }
            let is_running = version == running;
            kernels.push(KernelInfo {
                version,
                devel,
                running: is_running,
            });
        }
    }
    // Uruchomione jądro na początku listy, reszta alfabetycznie.
    kernels.sort_by(|a, b| b.running.cmp(&a.running).then(a.version.cmp(&b.version)));
    kernels
}

#[cfg(test)]
mod tests {
    /// Podgląd wykrywania na żywym systemie (ślady po sysextach mają zniknąć):
    /// `cargo test -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn wykrywanie_jader_na_zywym_systemie() {
        for kernel in super::detect() {
            println!(
                "{} devel={} running={}",
                kernel.version, kernel.devel, kernel.running
            );
        }
    }
}
