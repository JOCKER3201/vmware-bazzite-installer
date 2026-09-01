// Statyczna zawartość plików generowanych do wnętrza rozszerzenia:
// metadane sysext, jednostki systemd, fabryczne /etc/vmware,
// zapasowe konteksty SELinux dla mkfs.erofs.
//
// Treści oparte na sprawdzonym układzie pakietu AUR vmware-workstation
// (github.com/archlinux/aur, gałąź vmware-workstation) oraz praktyce
// projektów fedora-sysexts i Flatcar (wzorzec Upholds= dla sysextów).

/// Metadane rozszerzenia — nazwa pliku musi odpowiadać nazwie obrazu
/// (vmware.raw → extension-release.vmware). ID=_any: świadomy wybór
/// ekosystemu fedora-sysexts, zgodny z Universal Blue/Bazzite.
pub const EXTENSION_RELEASE: &str = "\
ID=_any
ARCHITECTURE=x86-64
SYSEXT_SCOPE=system
EXTENSION_RELOAD_MANAGER=1
";

/// Ładowanie modułów jądra. Celowo NIE używamy modules-load.d —
/// systemd-modules-load.service nie ma gwarantowanej kolejności względem
/// systemd-sysext.service i może wystartować przed scaleniem rozszerzenia.
/// %v = wersja uruchomionego jądra (uname -r): po aktualizacji jądra
/// jednostka wyłącza się warunkiem zamiast sypać błędami.
pub const UNIT_MODULES: &str = "\
[Unit]
Description=Ładowanie modułów jądra VMware (vmw_vmci, vmmon, vmnet)
ConditionPathExists=/usr/lib/modules/%v/misc/vmmon.ko

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=-/usr/sbin/modprobe vmw_vmci
ExecStart=-/usr/sbin/modprobe vmmon
ExecStart=-/usr/sbin/modprobe vmnet

[Install]
WantedBy=multi-user.target
";

pub const UNIT_NETWORKS_CONFIGURATION: &str = "\
[Unit]
Description=Generowanie domyślnej konfiguracji sieci VMware (/etc/vmware/networking)
ConditionPathExists=!/etc/vmware/networking
After=systemd-tmpfiles-setup.service

[Service]
Type=oneshot
RemainAfterExit=yes
UMask=0077
ExecStart=/usr/bin/vmware-networks --postinstall vmware-player,0,1
";

pub const UNIT_NETWORKS: &str = "\
[Unit]
Description=Usługi sieci wirtualnych VMware (vmnet)
Wants=vmware-networks-configuration.service
After=vmware-networks-configuration.service systemd-tmpfiles-setup.service network-pre.target

[Service]
Type=forking
ExecStartPre=-/usr/sbin/modprobe vmnet
ExecStart=/usr/bin/vmware-networks --start
ExecStop=/usr/bin/vmware-networks --stop

[Install]
WantedBy=multi-user.target
";

pub const UNIT_USBARBITRATOR: &str = "\
[Unit]
Description=Arbiter USB VMware (przekazywanie urządzeń USB do maszyn wirtualnych)
After=systemd-tmpfiles-setup.service

[Service]
ExecStartPre=-/usr/sbin/modprobe vmmon
ExecStart=/usr/lib/vmware/bin/vmware-usbarbitrator -f

[Install]
WantedBy=multi-user.target
";

/// Autostart usług po scaleniu rozszerzenia — wzorzec Flatcar/fedora-sysexts:
/// drop-in na multi-user.target z Upholds= zamiast systemctl enable
/// (dowiązań enable nie da się dostarczyć w sysext, bo żyją w /etc).
pub const UPHOLDS_DROPIN: &str = "\
[Unit]
Upholds=vmware-modules.service
Upholds=vmware-networks.service
Upholds=vmware-usbarbitrator.service
";

/// Zawartość /etc/vmware/config, którą normalnie generuje instalator VMware
/// (zestaw kluczy jak w pakiecie AUR; wartości product.* mogą być starsze niż
/// pakiet — VMware je toleruje, krytyczny jest libdir).
pub const ETC_CONFIG: &str = "\
.encoding = \"UTF-8\"
product.name = \"VMware Player\"
product.version = \"17.0.0\"
product.buildNumber = \"20800274\"
workstation.product.version = \"17.0.0\"
player.product.version = \"17.0.0\"
vix.config.version = \"1\"
bindir = \"/usr/bin\"
libdir = \"/usr/lib/vmware\"
vix.libdir = \"/usr/lib/vmware-vix\"
initscriptdir = \"/usr/lib/systemd/scripts\"
vmware.fullpath = \"/usr/bin/vmware\"
authd.fullpath = \"/usr/bin/vmware-authd\"
gksu.rootMethod = \"su\"
NETWORKING = \"yes\"
installerDefaults.autoSoftwareUpdateEnabled = \"no\"
installerDefaults.dataCollectionEnabled = \"no\"
installerDefaults.componentDownloadEnabled = \"no\"
installerDefaults.transferVersion = \"1\"
acceptOVFEULA = \"yes\"
acceptEULA = \"yes\"
";

pub const ETC_BOOTSTRAP: &str = "\
PREFIX=\"/usr\"
BINDIR=\"/usr/bin\"
SBINDIR=\"/usr/sbin\"
LIBDIR=\"/usr/lib\"
DATADIR=\"/usr/share\"
SYSCONFDIR=\"/etc\"
DOCDIR=\"/usr/share/doc\"
MANDIR=\"/usr/share/man\"
INCLUDEDIR=\"/usr/include\"
INITDIR=\"\"
INITSCRIPTDIR=\"/usr/lib/systemd/scripts\"
";

/// Zapasowe konteksty SELinux — używane tylko, gdy nie ma systemowego
/// /etc/selinux/targeted/contexts/files/file_contexts (preferowany).
/// libselinux wybiera wpis najbardziej szczegółowy, nie ostatni.
pub const FILE_CONTEXTS_FALLBACK: &str = "\
/usr(/.*)?\tsystem_u:object_r:usr_t:s0
/usr/bin(/.*)?\tsystem_u:object_r:bin_t:s0
/usr/lib(/.*)?\tsystem_u:object_r:lib_t:s0
/usr/lib/vmware/bin(/.*)?\tsystem_u:object_r:bin_t:s0
/usr/lib/modules(/.*)?\tsystem_u:object_r:modules_object_t:s0
/usr/lib/modules/[^/]+/modules\\..+\t--\tsystem_u:object_r:modules_dep_t:s0
/usr/lib/systemd/system(/.*)?\tsystem_u:object_r:systemd_unit_file_t:s0
";
