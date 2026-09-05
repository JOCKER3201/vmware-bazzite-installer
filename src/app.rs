// Interfejs graficzny: prosty kreator — plik .bundle → jądro → budowanie →
// gotowy obraz z instrukcją instalacji.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};

use crate::builder::{self, kernel::KernelInfo, BuildConfig, BuildEvent, BuildTask, STAGES};

#[derive(Clone, Copy, PartialEq)]
enum TaskKind {
    Build,
    Install,
}

pub struct InstallerApp {
    bundle_path: Option<PathBuf>,
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
    /// Jądro, dla którego zbudowano ostatni obraz, i czy jest uruchomione.
    built_kernel: Option<(String, bool)>,
    build_result: Option<Result<PathBuf, String>>,
    install_result: Option<Result<(), String>>,
}

impl InstallerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_pixels_per_point(1.1);
        Self {
            bundle_path: None,
            kernels: builder::kernel::detect(),
            selected_kernel: 0,
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

    /// Katalog wyjściowy: wybrany przez użytkownika albo katalog pliku .bundle.
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
                                    // Anulowanie to nie porażka — bez czerwonego
                                    // banera i mylącej diagnozy.
                                    self.cancelled = true;
                                } else {
                                    self.failed = true;
                                }
                                self.build_result = Some(result);
                            }
                            TaskKind::Install => {
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
                    // Pełny dziennik budowania — obok obrazu.
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

    fn section_bundle(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.strong("1. Plik instalacyjny VMware");
            ui.label(
                "Pobierz pełny instalator VMware Workstation dla Linuksa (plik .bundle) \
                 ze strony Broadcom, a następnie wskaż go tutaj.",
            );
            ui.horizontal(|ui| {
                if ui.button("📂 Wybierz plik .bundle…").clicked() {
                    let mut dialog = rfd::FileDialog::new()
                        .add_filter("Instalator VMware (*.bundle)", &["bundle"])
                        .set_title("Wybierz plik instalacyjny VMware");
                    if let Some(dir) = self.bundle_path.as_ref().and_then(|p| p.parent()) {
                        dialog = dialog.set_directory(dir);
                    }
                    if let Some(file) = dialog.pick_file() {
                        self.bundle_path = Some(file);
                    }
                }
                match &self.bundle_path {
                    Some(path) => {
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        let version = crate::util::guess_bundle_version(&name)
                            .map(|v| format!("  (wykryta wersja: {v})"))
                            .unwrap_or_default();
                        ui.label(RichText::new(format!("{name}{version}")).strong());
                    }
                    None => {
                        ui.label(RichText::new("nie wybrano pliku").italics().weak());
                    }
                }
            });
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
                        "⚠ Wersje 17.x zwykle NIE kompilują się na nowych jądrach (7.x) bez łatek.\n\
                         Zalecany jest pakiet 25H2/26H1 — albo wskaż łatane źródła/łatki w „Opcjach zaawansowanych”.",
                    );
                }
            }
        });
    }

    fn section_kernel(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.strong("2. Jądro docelowe");
            if self.kernels.is_empty() {
                ui.colored_label(
                    Color32::LIGHT_RED,
                    "Nie znaleziono żadnych jąder w /usr/lib/modules.",
                );
                return;
            }
            let selected_text = self
                .kernels
                .get(self.selected_kernel)
                .map(kernel_label)
                .unwrap_or_default();
            egui::ComboBox::from_label("wersja jądra")
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
                            "✔ Nagłówki jądra dostępne: {}",
                            kernel.build_dir().display()
                        ),
                    );
                } else {
                    ui.colored_label(
                        Color32::LIGHT_RED,
                        "✖ Brak nagłówków jądra (kernel-devel) — kompilacja modułów nie jest możliwa.\n\
                         Na Bazzite: sudo rpm-ostree install kernel-devel (i restart), albo wybierz inne jądro.",
                    );
                }
            }
            ui.label(
                RichText::new(
                    "Moduły vmmon i vmnet zostaną skompilowane dokładnie dla tej wersji jądra. \
                     Po aktualizacji jądra obraz trzeba zbudować ponownie.",
                )
                .weak(),
            );
        });
    }

    fn section_advanced(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Opcje zaawansowane")
            .default_open(false)
            .show(ui, |ui| {
                // Katalog wyjściowy
                ui.horizontal(|ui| {
                    ui.label("Katalog wyjściowy:");
                    if ui.button("Wybierz…").clicked() {
                        if let Some(dir) = rfd::FileDialog::new()
                            .set_title("Katalog na vmware.raw")
                            .pick_folder()
                        {
                            self.output_dir = Some(dir);
                        }
                    }
                    if let Some(dir) = self.output_dir.clone() {
                        ui.label(dir.display().to_string());
                        if ui
                            .button("✕")
                            .on_hover_text("Wróć do domyślnego (katalog pliku .bundle)")
                            .clicked()
                        {
                            self.output_dir = None;
                        }
                    } else {
                        match self.effective_output_dir() {
                            Some(dir) => {
                                ui.label(format!("{} (domyślny)", dir.display()));
                            }
                            None => {
                                ui.label(RichText::new("(katalog pliku .bundle)").weak());
                            }
                        }
                    }
                });
                // Łatane źródła modułów
                ui.horizontal(|ui| {
                    ui.label("Łatane źródła modułów:");
                    if ui.button("Wybierz katalog…").clicked() {
                        if let Some(dir) = rfd::FileDialog::new()
                            .set_title("Katalog ze źródłami vmmon-only/ i vmnet-only/")
                            .pick_folder()
                        {
                            self.custom_sources = Some(dir);
                        }
                    }
                    if let Some(dir) = &self.custom_sources {
                        ui.label(dir.display().to_string());
                        if ui.button("✕").on_hover_text("Wróć do źródeł z pakietu").clicked() {
                            self.custom_sources = None;
                        }
                    } else {
                        ui.label(RichText::new("(źródła z pakietu VMware)").weak());
                    }
                });
                ui.label(
                    RichText::new(
                        "Gdy źródła z pakietu nie kompilują się na nowym jądrze, wskaż tu katalog z:\n\
                         • źródłami forka vmware-host-modules (vmmon-only/ i vmnet-only/), np. philipl/vmware-host-modules, albo\n\
                         • łatkami vmmon.patch i vmnet.patch (np. checkout AUR vmware-workstation) — zostaną nałożone na źródła z pakietu.",
                    )
                    .weak(),
                );
                ui.separator();
                // Podpisywanie modułów (Secure Boot)
                ui.label("Podpisywanie modułów (tylko przy włączonym Secure Boot):");
                ui.horizontal(|ui| {
                    ui.label("Klucz prywatny (MOK):");
                    if ui.button("Wybierz…").clicked() {
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
                    ui.label("Certyfikat (DER):");
                    if ui.button("Wybierz…").clicked() {
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
            let button = egui::Button::new(RichText::new("🛠  Zbuduj vmware.raw").size(17.0))
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
                    ui.add_enabled(false, egui::Button::new("przerywanie…"));
                } else if ui.button("Anuluj").clicked() {
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
        ui.strong("Dziennik");
        egui::Frame::none()
            .fill(ui.visuals().extreme_bg_color)
            .inner_margin(8.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("dziennik")
                    .max_height(240.0)
                    .stick_to_bottom(true)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let start = self.log.len().saturating_sub(400);
                        if start > 0 {
                            ui.label(
                                RichText::new(format!(
                                    "… (starsze linie ukryto: {start}; pełny dziennik zostanie \
                                     zapisany obok obrazu jako vmware-sysext-build.log)"
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
                        RichText::new(format!("✅ Obraz gotowy: {}", raw.display())).size(15.0),
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
                                    "⚠ Obraz zbudowano dla jądra {kernel}, które nie jest teraz \
                                     uruchomione — VMware zadziała dopiero po restarcie do tego jądra."
                                ),
                            );
                        }
                    }
                    ui.add_space(4.0);
                    ui.strong("Instalacja rozszerzenia:");
                    let commands = builder::manual_commands(&raw);
                    ui.label(RichText::new(&commands).monospace().size(12.0));
                    ui.horizontal(|ui| {
                        if ui.button("📋 Skopiuj polecenia").clicked() {
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
                                egui::Button::new("🚀 Zainstaluj teraz (pkexec)"),
                            )
                            .clicked()
                        {
                            self.install_result = None;
                            self.task =
                                Some((TaskKind::Install, builder::spawn_install(raw.clone())));
                        }
                        if installing {
                            ui.spinner();
                            ui.label("czekam na autoryzację i instalację…");
                        }
                    });
                    ui.label(
                        RichText::new(
                            "Usługi VMware (moduły, sieci, USB) wystartują automatycznie po scaleniu \
                             — obraz zawiera drop-in Upholds= dla multi-user.target.",
                        )
                        .weak(),
                    );
                    match &self.install_result {
                        Some(Ok(())) => {
                            ui.colored_label(
                                Color32::from_rgb(120, 200, 120),
                                "✅ Zainstalowano. Uruchom program poleceniem „vmware” lub z menu aplikacji.",
                            );
                        }
                        Some(Err(err)) => {
                            ui.colored_label(
                                Color32::LIGHT_RED,
                                format!("Instalacja nie powiodła się: {err}"),
                            );
                        }
                        None => {}
                    }
                    ui.label(
                        RichText::new(
                            "Pamiętaj: po każdej aktualizacji jądra (rpm-ostree/bootc) zbuduj \
                             i zainstaluj obraz ponownie.",
                        )
                        .weak(),
                    );
                });
            }
            Err(error) => {
                if self.cancelled {
                    ui.group(|ui| {
                        ui.label(RichText::new("⏹ Budowanie przerwane przez użytkownika.").size(15.0));
                        ui.label(
                            RichText::new("Poprzedni obraz vmware.raw (jeśli istniał) pozostał nietknięty.")
                                .weak(),
                        );
                    });
                } else {
                    ui.group(|ui| {
                        ui.colored_label(
                            Color32::LIGHT_RED,
                            RichText::new("❌ Budowanie nie powiodło się").size(15.0),
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
        label.push_str("  (uruchomione)");
    }
    if !kernel.devel {
        label.push_str("  ⚠ brak kernel-devel");
    }
    label
}

impl eframe::App for InstallerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_task(ctx);

        egui::TopBottomPanel::top("naglowek").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.heading("Instalator VMware dla Bazzite — obraz systemd-sysext");
            ui.label(
                "Z pliku instalacyjnego VMware Workstation (.bundle) buduje gotowy obraz \
                 vmware.raw z modułami jądra — bez modyfikowania systemu.",
            );
            ui.add_space(6.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let busy = self.task.is_some();
                    ui.add_enabled_ui(!busy, |ui| {
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
