// Detection of installed kernel versions and header availability (kernel-devel).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[derive(Clone, Debug)]
pub struct KernelInfo {
    pub version: String,
    /// Whether headers for module compilation are available (…/build/Makefile).
    pub devel: bool,
    /// Whether this is the currently running kernel (uname -r).
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
            // A directory without kernel/, vmlinuz and build/ is not an
            // installed kernel but a leftover of a merged extension (vmmon/vmnet
            // modules of an old kernel from vmware.raw after a system update) —
            // skip it.
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
    // Running kernel first, the rest alphabetically.
    kernels.sort_by(|a, b| b.running.cmp(&a.running).then(a.version.cmp(&b.version)));
    kernels
}

#[cfg(test)]
mod tests {
    /// Preview of the detection on a live system (sysext leftovers must be gone):
    /// `cargo test -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn kernel_detection_on_live_system() {
        for kernel in super::detect() {
            println!(
                "{} devel={} running={}",
                kernel.version, kernel.devel, kernel.running
            );
        }
    }
}
