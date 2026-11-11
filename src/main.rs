// vmware-sysext-builder — graphical builder of a systemd-sysext image from a
// VMware Workstation package (.bundle) for Fedora Atomic systems (Bazzite).

mod app;
mod builder;
mod util;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1020.0, 740.0])
            .with_min_inner_size([840.0, 560.0])
            .with_title("VMware Installer — systemd-sysext (Bazzite)"),
        ..Default::default()
    };
    eframe::run_native(
        "vmware-sysext-builder",
        options,
        Box::new(|cc| Ok(Box::new(app::InstallerApp::new(cc)))),
    )
}
