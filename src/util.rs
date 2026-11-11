// Helper utilities: running commands with log streaming to the GUI
// and file-tree operations.

use anyhow::{bail, Context, Result};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::builder::BuildEvent;

pub fn log(tx: &Sender<BuildEvent>, msg: impl Into<String>) {
    let _ = tx.send(BuildEvent::Log(msg.into()));
}

/// Runs a command, forwarding its stdout/stderr to the log line by line.
/// Once the cancel flag is set, the process is killed.
pub fn run_cmd<I, S>(
    tx: &Sender<BuildEvent>,
    cancel: &Arc<AtomicBool>,
    cwd: Option<&Path>,
    envs: &[(&str, String)],
    program: &str,
    args: I,
) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<OsString> = args.into_iter().map(|a| a.as_ref().to_os_string()).collect();
    let shown = format!(
        "$ {} {}",
        program,
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    );
    log(tx, shown.clone());

    let mut cmd = Command::new(program);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group: cancellation must also reach grandchildren
        // (compilers spawned by make, the package extractor, etc.).
        .process_group(0);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to run `{program}` — is it installed?"))?;

    let mut readers: Vec<thread::JoinHandle<()>> = Vec::new();
    if let Some(stream) = child.stdout.take() {
        readers.push(pipe_to_log(tx.clone(), stream));
    }
    if let Some(stream) = child.stderr.take() {
        readers.push(pipe_to_log(tx.clone(), stream));
    }

    let mut cancelled = false;
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            let pgid = child.id() as i32;
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
            let _ = child.kill();
            break child.wait()?;
        }
        match child.try_wait()? {
            Some(status) => break status,
            None => thread::sleep(Duration::from_millis(60)),
        }
    };
    for handle in readers {
        let _ = handle.join();
    }
    if cancelled {
        bail!("Build cancelled by the user");
    }
    if !status.success() {
        bail!("Command failed ({status}): {shown}");
    }
    Ok(())
}

fn pipe_to_log(tx: Sender<BuildEvent>, stream: impl Read + Send + 'static) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    while matches!(buf.last(), Some(b'\n') | Some(b'\r')) {
                        buf.pop();
                    }
                    let _ = tx.send(BuildEvent::Log(String::from_utf8_lossy(&buf).into_owned()));
                }
            }
        }
    })
}

/// Runs a command and returns its stdout (trimmed) — for short reads
/// like modinfo/sha256sum, without streaming to the log.
pub fn capture_cmd<I, S>(program: &str, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("Failed to run `{program}`"))?;
    if !output.status.success() {
        bail!(
            "Command `{program}` failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// VMware version from a file name like
/// VMware-Workstation-Full-25H2-24995812.x86_64.bundle → "25H2".
pub fn guess_bundle_version(name: &str) -> Option<String> {
    let rest = name.split("VMware-Workstation-Full-").nth(1)?;
    let version = rest.split('-').next()?;
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

/// Is a program with the given name available in PATH?
pub fn cmd_exists(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path).any(|dir| {
                let candidate = dir.join(name);
                fs::metadata(&candidate)
                    .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Copies a file tree, preserving file permissions and symbolic links.
/// The contents of the source directory are merged into the destination.
/// Directories are created with the current umask — the tree goes through
/// normalize_dir_modes before the image is packed.
pub fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(src)
        .with_context(|| format!("Cannot access {}", src.display()))?;
    let file_type = meta.file_type();
    if file_type.is_dir() {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else if file_type.is_symlink() {
        let target = fs::read_link(src)?;
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        let _ = fs::remove_file(dst);
        symlink(&target, dst)
            .with_context(|| format!("Symlinking {} → {}", dst.display(), target.display()))?;
    } else {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dst)
            .with_context(|| format!("Copying {} → {}", src.display(), dst.display()))?;
    }
    Ok(())
}

/// Writes a text file with the given permissions, creating parent directories.
pub fn write_file(path: &Path, content: &str, mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content).with_context(|| format!("Writing {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

/// Replaces all occurrences of a string in a file (byte-safe).
/// Returns true when a change was made. The file mode is preserved.
pub fn replace_in_file(path: &Path, from: &str, to: &str) -> Result<bool> {
    let data = fs::read(path).with_context(|| format!("Reading {}", path.display()))?;
    let needle = from.as_bytes();
    if needle.is_empty() || !data.windows(needle.len()).any(|w| w == needle) {
        return Ok(false);
    }
    let mut output = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i..].starts_with(needle) {
            output.extend_from_slice(to.as_bytes());
            i += needle.len();
        } else {
            output.push(data[i]);
            i += 1;
        }
    }
    fs::write(path, output).with_context(|| format!("Writing {}", path.display()))?;
    Ok(true)
}

/// Recursively finds the first file with the given name.
pub fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(root).ok()?;
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name() == OsStr::new(name) && path.is_file() {
            return Some(path);
        }
        if let Ok(ft) = entry.file_type() {
            if ft.is_dir() {
                dirs.push(path);
            }
        }
    }
    for dir in dirs {
        if let Some(found) = find_file(&dir, name) {
            return Some(found);
        }
    }
    None
}

/// "Symlink farm": a mirror of a tree in which directories are real
/// and every file is a symlink to the original (for depmod).
pub fn symlink_farm(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            symlink_farm(&src_path, &dst_path)?;
        } else {
            let _ = fs::remove_file(&dst_path);
            symlink(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Sets 0755 on all directories of the tree — makes the image independent
/// of the user's umask (with umask 077 directories would be 0700 and, after
/// merging, a regular user could not enter /usr/lib/vmware).
pub fn normalize_dir_modes(root: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(root)?;
    if !meta.is_dir() {
        return Ok(());
    }
    fs::set_permissions(root, fs::Permissions::from_mode(0o755))?;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            normalize_dir_modes(&entry.path())?;
        }
    }
    Ok(())
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
