// Graphical interface: a simple wizard — .bundle file → kernel → build →
// finished image with installation instructions.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};

use crate::builder::{
    self, installed::InstalledImage, kernel::KernelInfo, BuildConfig, BuildEvent, BuildTask, STAGES,
};

#[derive(Clone, Copy, PartialEq)]
enum TaskKind {
    Build,
    Install,
}

pub struct InstallerApp {
    bundle_path: Option<PathBuf>,
    /// Whether bundle_path comes from autodetection (the installed image's
    /// manifest) rather than from the user's manual choice.
    bundle_auto: bool,
    /// Manifest of the currently merged extension (if one is installed).
    installed: Option<InstalledImage>,
    /// System name from /etc/os-release, e.g. "Bazzite 45".
    os_label: Option<String>,
    kernels: Vec<KernelInfo>,
    selected_kernel: usize,
    output_dir: Option<PathBuf>,
    custom_sources: Option<PathBuf>,
    sign_key: Option<PathBuf>,
    sign_cert: Option<PathBuf>,

    task: Option<(TaskKind, BuildTask)>,
    log: Vec<String>,
    warnings: Vec<String>,
    active_stage: Option<usize>,
    failed: bool,
    cancelled: bool,
    /// Kernel the last image was built for, and whether it is running.
    built_kernel: Option<(String, bool)>,
    build_result: Option<Result<PathBuf, String>>,
    install_result: Option<Result<(), String>>,
}

impl InstallerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_pixels_per_point(1.1);
        let kernels = builder::kernel::detect();
        // Autodetection: target kernel = the running one, and the .bundle
        // package — the one from the installed image's manifest (if it can
        // be located).
        let selected_kernel = kernels.iter().position(|k| k.running).unwrap_or(0);
        let installed = builder::installed::read();
        let bundle_path = installed.as_ref().and_then(builder::installed::locate_bundle);
        Self {
            bundle_auto: bundle_path.is_some(),
            bundle_path,
            installed,
            os_label: builder::installed::os_label(),
            kernels,
            selected_kernel,
            output_dir: None,
            custom_sources: None,
            sign_key: None,
            sign_cert: None,
            task: None,
            log: Vec::new(),
            warnings: Vec::new(),
            active_stage: None,
            failed: false,
            cancelled: false,
            built_kernel: None,
            build_result: None,
            install_result: None,
        }
    }

    /// Output directory: the one chosen by the user, or the .bundle file's directory.
    fn effective_output_dir(&self) -> Option<PathBuf> {
        self.output_dir.clone().or_else(|| {
            self.bundle_path
                .as_ref()
                .and_then(|b| b.parent().map(|p| p.to_path_buf()))
        })
    }

    fn poll_task(&mut self, ctx: &egui::Context) {
        let mut finished = false;
        if let Some((kind, task)) = &mut self.task {
            let kind = *kind;
            while let Ok(event) = task.rx.try_recv() {
                match event {
                    BuildEvent::Log(line) => self.log.push(line),
                    BuildEvent::Warning(warning) => {
                        self.log.push(format!("⚠ {warning}"));
                        self.warnings.push(warning);
                    }
                    BuildEvent::Stage(index) => self.active_stage = Some(index),
                    BuildEvent::Done(result) => {
                        match kind {
                            TaskKind::Build => {
                                if result.is_ok() {
                                    self.active_stage = None;
                                } else if task.cancel.load(Ordering::Relaxed) {
                                    // Cancellation is not a failure — no red
                                    // banner and no misleading diagnosis.
                                    self.cancelled = true;
                                } else {
                                    self.failed = true;
                                }
                                self.build_result = Some(result);
                            }
                            TaskKind::Install => {
                                if result.is_ok() {
                                    // After the merge the manifest in /usr already
                                    // describes the new image — refresh the status banner.
                                    self.installed = builder::installed::read();
                                }
                                self.install_result = Some(result.map(|_| ()));
                            }
                        }
                        finished = true;
                    }
                }
            }
            if !finished {
                ctx.request_repaint_after(Duration::from_millis(120));
            }
        }
        if finished {
            if let Some((kind, mut task)) = self.task.take() {
                if let Some(handle) = task.handle.take() {
                    let _ = handle.join();
                }
                if kind == TaskKind::Build {
                    // The full build log — next to the image.
                    if let Some(dir) = self.effective_output_dir() {
                        let _ = fs::write(dir.join("vmware-sysext-build.log"), self.log.join("\n"));
                    }
                }
            }
        }
    }

    fn start_build(&mut self) {
        let (Some(bundle), Some(output_dir)) = (self.bundle_path.clone(), self.effective_output_dir())
        else {
            return;
        };
        let Some(kernel) = self.kernels.get(self.selected_kernel) else {
            return;
        };
        self.log.clear();
        self.warnings.clear();
        self.build_result = None;
        self.install_result = None;
        self.failed = false;
        self.cancelled = false;
        self.active_stage = Some(0);
        self.built_kernel = Some((kernel.version.clone(), kernel.running));
        let cfg = BuildConfig {
            bundle,
            kernel: kernel.version.clone(),
            custom_sources: self.custom_sources.clone(),
            sign_key: self.sign_key.clone(),
            sign_cert: self.sign_cert.clone(),
            output_dir,
        };
        self.task = Some((TaskKind::Build, builder::spawn_build(cfg)));
    }

    /// State of the installed extension relative to the running system.
    /// After a system update (new kernel, e.g. Bazzite F44 → F45) the modules
    /// of the old image no longer match — the banner offers a one-click
    /// rebuild: the detected .bundle package + the running kernel.
    fn section_status(&mut self, ui: &mut egui::Ui) {
        let Some(installed) = self.installed.clone() else {
            // No installed image — the plain wizard, no banner.
            return;
        };
        let Some(running) = self.kernels.iter().find(|k| k.running).cloned() else {
            return;
        };
        if builder::installed::modules_present(&running.version) {
            ui.group(|ui| {
                ui.colored_label(
                    Color32::from_rgb(120, 200, 120),
                    format!(
                        "✔ The installed vmware.raw image matches the running kernel {} — \
                         no rebuild needed.",
                        running.version
                    ),
                );
            });
            ui.add_space(8.0);
            return;
        }
        ui.group(|ui| {
            let os = self
                .os_label
                .clone()
                .unwrap_or_else(|| "the system".to_string());
            let built = installed
                .built_utc
                .as_deref()
                .map(|d| format!(", built {d}"))
                .unwrap_or_default();
            ui.colored_label(
                Color32::from_rgb(230, 170, 70),
                format!(
                    "⚠ System update detected: {os} is running kernel {}, while the installed \
                     image was built for {}{built} (package {}).\n\
                     The VMware modules are not available for the current kernel — the image \
                     needs a rebuild.",
                    running.version, installed.kernel, installed.bundle_name
                ),
            );
            ui.add_space(4.0);
            if self.bundle_path.is_some() {
                let ready = running.devel && self.task.is_none();
                if ui
                    .add_enabled(
                        ready,
                        egui::Button::new(
                            RichText::new("🔁 Rebuild for the running kernel").size(15.0),
                        ),
                    )
                    .clicked()
                {
                    if let Some(index) = self.kernels.iter().position(|k| k.running) {
                        self.selected_kernel = index;
                    }
                    self.start_build();
                }
                if !running.devel {
                    ui.colored_label(
                        Color32::LIGHT_RED,
                        "✖ Headers for the running kernel are missing (kernel-devel) — see section 2.",
                    );
                }
            } else {
                ui.colored_label(
                    Color32::LIGHT_RED,
                    format!(
                        "Package {} was not found — select the .bundle file in section 1, \
                         then build the image.",
                        installed.bundle_name
                    ),
                );
            }
        });
        ui.add_space(8.0);
    }

    fn section_bundle(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.strong("1. VMware installer file");
            ui.label(
                "Download the full VMware Workstation installer for Linux (a .bundle file) \
                 from the Broadcom site, then select it here.",
            );
            ui.horizontal(|ui| {
                if ui.button("📂 Select .bundle file…").clicked() {
                    let mut dialog = rfd::FileDialog::new()
                        .add_filter("VMware installer (*.bundle)", &["bundle"])
                        .set_title("Select the VMware installer file");
                    if let Some(dir) = self.bundle_path.as_ref().and_then(|p| p.parent()) {
                        dialog = dialog.set_directory(dir);
                    }
                    if let Some(file) = dialog.pick_file() {
                        self.bundle_path = Some(file);
                        self.bundle_auto = false;
                    }
                }
                match &self.bundle_path {
                    Some(path) => {
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        let version = crate::util::guess_bundle_version(&name)
                            .map(|v| format!("  (detected version: {v})"))
                            .unwrap_or_default();
                        ui.label(RichText::new(format!("{name}{version}")).strong());
                    }
                    None => {
                        ui.label(RichText::new("no file selected").italics().weak());
                    }
                }
            });
            if self.bundle_auto {
                ui.label(
                    RichText::new(
                        "Package located automatically based on the installed image — \
                         you can select a different one.",
                    )
                    .weak(),
                );
            }
            if let Some(version) = self
                .bundle_path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .and_then(|n| crate::util::guess_bundle_version(&n))
            {
                if version.starts_with("17.") {
                    ui.colored_label(
                        Color32::from_rgb(230, 170, 70),
                        "⚠ 17.x versions usually do NOT compile on new kernels (7.x) without patches.\n\
                         A 25H2/26H1 package is recommended — or point to patched sources/patches under \"Advanced options\".",
                    );
                }
            }
        });
    }

    fn section_kernel(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.strong("2. Target kernel");
            if self.kernels.is_empty() {
                ui.colored_label(
                    Color32::LIGHT_RED,
                    "No kernels found in /usr/lib/modules.",
                );
                return;
            }
            let selected_text = self
                .kernels
                .get(self.selected_kernel)
                .map(kernel_label)
                .unwrap_or_default();
            egui::ComboBox::from_label("kernel version")
                .width(420.0)
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    for (index, kernel) in self.kernels.iter().enumerate() {
                        ui.selectable_value(&mut self.selected_kernel, index, kernel_label(kernel));
                    }
                });
            if let Some(kernel) = self.kernels.get(self.selected_kernel) {
                if kernel.devel {
                    ui.colored_label(
                        Color32::from_rgb(120, 200, 120),
                        format!(
                            "✔ Kernel headers available: {}",
                            kernel.build_dir().display()
                        ),
                    );
                } else {
                    ui.colored_label(
                        Color32::LIGHT_RED,
                        "✖ Kernel headers missing (kernel-devel) — module compilation is not possible.\n\
                         On Bazzite: sudo rpm-ostree install kernel-devel (and reboot), or select another kernel.",
                    );
                }
            }
            ui.label(
                RichText::new(
                    "The vmmon and vmnet modules will be compiled exactly for this kernel \
                     version. After a kernel update the image must be rebuilt.",
                )
                .weak(),
            );
        });
    }

    fn section_advanced(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Advanced options")
            .default_open(false)
            .show(ui, |ui| {
                // Output directory
                ui.horizontal(|ui| {
                    ui.label("Output directory:");
                    if ui.button("Select…").clicked() {
                        if let Some(dir) = rfd::FileDialog::new()
                            .set_title("Directory for vmware.raw")
                            .pick_folder()
                        {
                            self.output_dir = Some(dir);
                        }
                    }
                    if let Some(dir) = self.output_dir.clone() {
                        ui.label(dir.display().to_string());
                        if ui
                            .button("✕")
                            .on_hover_text("Back to the default (the .bundle file's directory)")
                            .clicked()
                        {
                            self.output_dir = None;
                        }
                    } else {
                        match self.effective_output_dir() {
                            Some(dir) => {
                                ui.label(format!("{} (default)", dir.display()));
                            }
                            None => {
                                ui.label(RichText::new("(the .bundle file's directory)").weak());
                            }
                        }
                    }
                });
                // Patched module sources
                ui.horizontal(|ui| {
                    ui.label("Patched module sources:");
                    if ui.button("Select directory…").clicked() {
                        if let Some(dir) = rfd::FileDialog::new()
                            .set_title("Directory with vmmon-only/ and vmnet-only/ sources")
                            .pick_folder()
                        {
                            self.custom_sources = Some(dir);
                        }
                    }
                    if let Some(dir) = &self.custom_sources {
                        ui.label(dir.display().to_string());
                        if ui.button("✕").on_hover_text("Back to the package sources").clicked() {
                            self.custom_sources = None;
                        }
                    } else {
                        ui.label(RichText::new("(sources from the VMware package)").weak());
                    }
                });
                ui.label(
                    RichText::new(
                        "When the package sources do not compile on a new kernel, point this at a directory with:\n\
                         • sources of a vmware-host-modules fork (vmmon-only/ and vmnet-only/), e.g. philipl/vmware-host-modules, or\n\
                         • the vmmon.patch and vmnet.patch patches (e.g. an AUR vmware-workstation checkout) — they will be applied to the package sources.",
                    )
                    .weak(),
                );
                ui.separator();
                // Module signing (Secure Boot)
                ui.label("Module signing (only with Secure Boot enabled):");
                ui.horizontal(|ui| {
                    ui.label("Private key (MOK):");
                    if ui.button("Select…").clicked() {
                        if let Some(file) = rfd::FileDialog::new().pick_file() {
                            self.sign_key = Some(file);
                        }
                    }
                    if let Some(path) = &self.sign_key {
                        ui.label(path.display().to_string());
                        if ui.button("✕").clicked() {
                            self.sign_key = None;
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Certificate (DER):");
                    if ui.button("Select…").clicked() {
                        if let Some(file) = rfd::FileDialog::new().pick_file() {
                            self.sign_cert = Some(file);
                        }
                    }
                    if let Some(path) = &self.sign_cert {
                        ui.label(path.display().to_string());
                        if ui.button("✕").clicked() {
                            self.sign_cert = None;
                        }
                    }
                });
            });
    }

    fn section_build(&mut self, ui: &mut egui::Ui) {
        let kernel_ok = self
            .kernels
            .get(self.selected_kernel)
            .map(|k| k.devel)
            .unwrap_or(false);
        let ready = self.bundle_path.is_some() && kernel_ok && self.task.is_none();

        ui.horizontal(|ui| {
            let button = egui::Button::new(RichText::new("🛠  Build vmware.raw").size(17.0))
                .min_size(egui::vec2(230.0, 36.0));
            if ui.add_enabled(ready, button).clicked() {
                self.start_build();
            }
            if let Some((TaskKind::Build, task)) = self
                .task
                .as_ref()
                .filter(|(kind, _)| *kind == TaskKind::Build)
            {
                ui.spinner();
                if task.cancel.load(Ordering::Relaxed) {
                    ui.add_enabled(false, egui::Button::new("cancelling…"));
                } else if ui.button("Cancel").clicked() {
                    task.cancel.store(true, Ordering::Relaxed);
                }
            }
        });

        if self.active_stage.is_some() || self.build_result.is_some() {
            ui.add_space(4.0);
            let active = self.active_stage;
            let build_ok = matches!(&self.build_result, Some(Ok(_)));
            for (index, name) in STAGES.iter().enumerate() {
                let (icon, color) = if self.failed && active == Some(index) {
                    ("✖", Color32::LIGHT_RED)
                } else if active == Some(index) && self.task.is_some() {
                    ("⏳", Color32::from_rgb(230, 200, 90))
                } else if build_ok || active.is_some_and(|a| index < a) {
                    ("✔", Color32::from_rgb(120, 200, 120))
                } else {
                    ("•", Color32::GRAY)
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon).color(color));
                    ui.label(*name);
                });
            }
        }
    }

    fn section_log(&mut self, ui: &mut egui::Ui) {
        if self.log.is_empty() {
            return;
        }
        ui.strong("Log");
        egui::Frame::none()
            .fill(ui.visuals().extreme_bg_color)
            .inner_margin(8.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("build_log")
                    .max_height(240.0)
                    .stick_to_bottom(true)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let start = self.log.len().saturating_sub(400);
                        if start > 0 {
                            ui.label(
                                RichText::new(format!(
                                    "… ({start} older lines hidden; the full log will be \
                                     saved next to the image as vmware-sysext-build.log)"
                                ))
                                .weak()
                                .size(11.0),
                            );
                        }
                        for line in &self.log[start..] {
                            ui.label(RichText::new(line).monospace().size(12.0));
                        }
                    });
            });
    }

    fn section_result(&mut self, ui: &mut egui::Ui) {
        let Some(result) = self.build_result.clone() else {
            return;
        };
        match result {
            Ok(raw) => {
                ui.group(|ui| {
                    ui.colored_label(
                        Color32::from_rgb(120, 200, 120),
                        RichText::new(format!("✅ Image ready: {}", raw.display())).size(15.0),
                    );
                    for warning in &self.warnings {
                        ui.colored_label(
                            Color32::from_rgb(230, 170, 70),
                            format!("⚠ {warning}"),
                        );
                    }
                    if let Some((kernel, running)) = &self.built_kernel {
                        if !running {
                            ui.colored_label(
                                Color32::from_rgb(230, 170, 70),
                                format!(
                                    "⚠ The image was built for kernel {kernel}, which is not \
                                     currently running — VMware will only work after rebooting \
                                     into that kernel."
                                ),
                            );
                        }
                    }
                    ui.add_space(4.0);
                    ui.strong("Installing the extension:");
                    let commands = builder::manual_commands(&raw);
                    ui.label(RichText::new(&commands).monospace().size(12.0));
                    ui.horizontal(|ui| {
                        if ui.button("📋 Copy commands").clicked() {
                            ui.output_mut(|out| out.copied_text = commands.clone());
                        }
                        let installing = self
                            .task
                            .as_ref()
                            .map(|(kind, _)| *kind == TaskKind::Install)
                            .unwrap_or(false);
                        if ui
                            .add_enabled(
                                self.task.is_none(),
                                egui::Button::new("🚀 Install now (pkexec)"),
                            )
                            .clicked()
                        {
                            self.install_result = None;
                            self.task =
                                Some((TaskKind::Install, builder::spawn_install(raw.clone())));
                        }
                        if installing {
                            ui.spinner();
                            ui.label("waiting for authorization and installation…");
                        }
                    });
                    ui.label(
                        RichText::new(
                            "The VMware services (modules, networks, USB) will start automatically \
                             after the merge — the image ships an Upholds= drop-in for multi-user.target.",
                        )
                        .weak(),
                    );
                    match &self.install_result {
                        Some(Ok(())) => {
                            ui.colored_label(
                                Color32::from_rgb(120, 200, 120),
                                "✅ Installed. Launch the program with the \"vmware\" command or from the application menu.",
                            );
                        }
                        Some(Err(err)) => {
                            ui.colored_label(
                                Color32::LIGHT_RED,
                                format!("Installation failed: {err}"),
                            );
                        }
                        None => {}
                    }
                    ui.label(
                        RichText::new(
                            "Remember: after every kernel update (rpm-ostree/bootc) build \
                             and install the image again.",
                        )
                        .weak(),
                    );
                });
            }
            Err(error) => {
                if self.cancelled {
                    ui.group(|ui| {
                        ui.label(RichText::new("⏹ Build cancelled by the user.").size(15.0));
                        ui.label(
                            RichText::new("The previous vmware.raw image (if any) was left untouched.")
                                .weak(),
                        );
                    });
                } else {
                    ui.group(|ui| {
                        ui.colored_label(
                            Color32::LIGHT_RED,
                            RichText::new("❌ Build failed").size(15.0),
                        );
                        ui.label(RichText::new(error).monospace().size(12.0));
                    });
                }
            }
        }
    }
}

fn kernel_label(kernel: &KernelInfo) -> String {
    let mut label = kernel.version.clone();
    if kernel.running {
        label.push_str("  (running)");
    }
    if !kernel.devel {
        label.push_str("  ⚠ kernel-devel missing");
    }
    label
}

impl eframe::App for InstallerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_task(ctx);

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.heading("VMware installer for Bazzite — systemd-sysext image");
            ui.label(
                "Builds a ready-to-use vmware.raw image with kernel modules from a VMware \
                 Workstation installer file (.bundle) — without modifying the system.",
            );
            ui.add_space(6.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let busy = self.task.is_some();
                    ui.add_enabled_ui(!busy, |ui| {
                        self.section_status(ui);
                        self.section_bundle(ui);
                        ui.add_space(8.0);
                        self.section_kernel(ui);
                        ui.add_space(8.0);
                        self.section_advanced(ui);
                    });
                    ui.add_space(12.0);
                    self.section_build(ui);
                    ui.add_space(8.0);
                    self.section_log(ui);
                    ui.add_space(8.0);
                    self.section_result(ui);
                    ui.add_space(12.0);
                });
        });
    }
}
