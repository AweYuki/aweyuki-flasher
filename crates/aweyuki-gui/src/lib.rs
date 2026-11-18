use aweyuki_core::boot_avb::patch_vbmeta_verity_flags;
use aweyuki_core::demo_samples::{generate_sample_rom, SampleKind};
use aweyuki_core::detector::{
    detect_from_bytes, detect_from_path, format_bytes_human, DetectedFormat, DetectedRomPackage,
    PartitionTargetMode, RomPackageKind,
};
use aweyuki_core::error::{FlashError, Result as CoreResult};
use aweyuki_core::planner::{
    build_flash_plan, export_script, DeviceMode, DeviceTargetInfo, FlashOptions, FlashPlan,
    FlashStage, ScriptFormat, SlotSelection, SuperFlashMode,
};
use aweyuki_core::reader::SourceReader;
use aweyuki_core::simulator::{
    execute_flash_plan, DeviceSimulator, FlashExecutionReport, FlashTransport, SimulatorProfile,
};
use aweyuki_core::sparse::{auto_split_image, SparseSplitMeta, VisualSegmentKind};
use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, RichText, Stroke, Vec2,
};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;

const FONT_INTER: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
const FONT_DEJAVU: &[u8] = include_bytes!("../../../assets/fonts/DejaVuSans.ttf");
const FONT_MONO: &[u8] = include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf");

// Obsidian Palette
const BG_CANVAS: Color32 = Color32::from_rgb(9, 10, 15);
const BG_CHROME: Color32 = Color32::from_rgb(14, 16, 23);
const BG_CARD: Color32 = Color32::from_rgb(17, 20, 29);
const BG_SURFACE: Color32 = Color32::from_rgb(23, 27, 39);
const BORDER_HAIR: Color32 = Color32::from_rgb(34, 39, 54);
const TEXT_PRIMARY: Color32 = Color32::from_rgb(241, 245, 249);
const TEXT_MUTED: Color32 = Color32::from_rgb(148, 163, 184);
const TEXT_DIM: Color32 = Color32::from_rgb(100, 116, 139);
const ACCENT_CYAN: Color32 = Color32::from_rgb(56, 189, 248);
const ACCENT_GREEN: Color32 = Color32::from_rgb(34, 197, 94);
const ACCENT_AMBER: Color32 = Color32::from_rgb(245, 158, 11);
const ACCENT_VIOLET: Color32 = Color32::from_rgb(168, 85, 247);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lang {
    Vi,
    En,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkspaceTab {
    Partitions,
    ChunkSplit,
    ExportScript,
    FlashConsole,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TransportMode {
    Simulator,
    HardwareUsb,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MaxSizePreset {
    DeviceDefault,
    SplitDemo256K,
    Mb64,
    Mb128,
    Mb256,
    Mb512,
}

impl MaxSizePreset {
    fn all() -> &'static [Self] {
        &[
            Self::DeviceDefault,
            Self::SplitDemo256K,
            Self::Mb64,
            Self::Mb128,
            Self::Mb256,
            Self::Mb512,
        ]
    }

    fn label(&self, lang: Lang) -> &'static str {
        match (self, lang) {
            (Self::DeviceDefault, Lang::Vi) => "Theo thiết bị (getvar)",
            (Self::DeviceDefault, Lang::En) => "Device Default (getvar)",
            (Self::SplitDemo256K, Lang::Vi) => "256 KB (Demo chia chunk)",
            (Self::SplitDemo256K, Lang::En) => "256 KB (Split Demo)",
            (Self::Mb64, _) => "64 MB (OTG an toàn)",
            (Self::Mb128, _) => "128 MB",
            (Self::Mb256, _) => "256 MB",
            (Self::Mb512, _) => "512 MB",
        }
    }

    fn bytes(&self, device_max: u64) -> u64 {
        match self {
            Self::DeviceDefault => device_max,
            Self::SplitDemo256K => 256 * 1024,
            Self::Mb64 => 64 * 1024 * 1024,
            Self::Mb128 => 128 * 1024 * 1024,
            Self::Mb256 => 256 * 1024 * 1024,
            Self::Mb512 => 512 * 1024 * 1024,
        }
    }
}

enum BgMessage {
    Log(String),
    Progress(usize, usize, String),
    Finished(Result<FlashExecutionReport, String>),
}

struct NativeGuiTransport {
    serial: Option<String>,
    temp_seq: u64,
}

impl NativeGuiTransport {
    fn fastboot_cmd(&self, args: &[&str]) -> Command {
        let bin = if std::env::var_os("TERMUX_VERSION").is_some()
            && Command::new("termux-fastboot").arg("--version").output().is_ok()
        {
            "termux-fastboot"
        } else {
            "fastboot"
        };
        let mut c = Command::new(bin);
        if let Some(s) = &self.serial {
            c.arg("-s").arg(s);
        }
        c.args(args);
        c
    }

    fn adb_cmd(&self, args: &[&str]) -> Command {
        let bin = if std::env::var_os("TERMUX_VERSION").is_some()
            && Command::new("termux-adb").arg("--version").output().is_ok()
        {
            "termux-adb"
        } else {
            "adb"
        };
        let mut c = Command::new(bin);
        if let Some(s) = &self.serial {
            c.arg("-s").arg(s);
        }
        c.args(args);
        c
    }

    fn run_stream(&self, mut cmd: Command, log: &mut dyn FnMut(&str)) -> CoreResult<()> {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| FlashError::Transport(format!("Failed to launch fastboot/adb: {e}")))?;
        if let Some(out) = child.stdout.take() {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    log(&format!("  {line}"));
                }
            }
        }
        if let Some(err) = child.stderr.take() {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    log(&format!("  {line}"));
                }
            }
        }
        let st = child.wait().map_err(|e| FlashError::Transport(e.to_string()))?;
        if !st.success() {
            return Err(FlashError::Transport(format!("Process exited with {st}")));
        }
        Ok(())
    }
}

impl FlashTransport for NativeGuiTransport {
    fn query_device(&mut self) -> CoreResult<DeviceTargetInfo> {
        query_first_real_device()
            .ok_or_else(|| FlashError::Transport("No USB Fastboot/ADB device connected".into()))
    }

    fn flash_blob(
        &mut self,
        partition: &str,
        data: &[u8],
        disable_verity: bool,
        disable_verification: bool,
        log: &mut dyn FnMut(&str),
    ) -> CoreResult<()> {
        self.temp_seq += 1;
        let path = std::env::temp_dir().join(format!(
            "aweyuki_gui_{}_{}_{}.img",
            std::process::id(),
            partition,
            self.temp_seq
        ));
        std::fs::write(&path, data)?;
        let pstr = path.to_string_lossy().to_string();
        let mut args = Vec::new();
        if disable_verity {
            args.push("--disable-verity");
        }
        if disable_verification {
            args.push("--disable-verification");
        }
        args.push("flash");
        args.push(partition);
        args.push(&pstr);
        log(&format!("> fastboot {}", args.join(" ")));
        let res = self.run_stream(self.fastboot_cmd(&args), log);
        let _ = std::fs::remove_file(&path);
        res
    }

    fn reboot_fastbootd(&mut self, log: &mut dyn FnMut(&str)) -> CoreResult<()> {
        log("> fastboot reboot fastboot");
        self.run_stream(self.fastboot_cmd(&["reboot", "fastboot"]), log)
    }

    fn delete_logical(&mut self, name: &str, log: &mut dyn FnMut(&str)) -> CoreResult<()> {
        log(&format!("> fastboot delete-logical-partition {name}"));
        let _ = self.run_stream(
            self.fastboot_cmd(&["delete-logical-partition", name]),
            log,
        );
        Ok(())
    }

    fn create_logical(
        &mut self,
        name: &str,
        size_bytes: u64,
        log: &mut dyn FnMut(&str),
    ) -> CoreResult<()> {
        let sz = size_bytes.to_string();
        log(&format!("> fastboot create-logical-partition {name} {sz}"));
        self.run_stream(
            self.fastboot_cmd(&["create-logical-partition", name, &sz]),
            log,
        )
    }

    fn resize_logical(
        &mut self,
        name: &str,
        size_bytes: u64,
        log: &mut dyn FnMut(&str),
    ) -> CoreResult<()> {
        let sz = size_bytes.to_string();
        log(&format!("> fastboot resize-logical-partition {name} {sz}"));
        self.run_stream(
            self.fastboot_cmd(&["resize-logical-partition", name, &sz]),
            log,
        )
    }

    fn set_active_slot(&mut self, slot: &str, log: &mut dyn FnMut(&str)) -> CoreResult<()> {
        log(&format!("> fastboot set_active {slot}"));
        self.run_stream(self.fastboot_cmd(&["set_active", slot]), log)
    }

    fn reboot_sideload(&mut self, log: &mut dyn FnMut(&str)) -> CoreResult<()> {
        log("> adb reboot sideload");
        self.run_stream(self.adb_cmd(&["reboot", "sideload"]), log)
    }

    fn adb_sideload(
        &mut self,
        package_name: &str,
        data: &[u8],
        log: &mut dyn FnMut(&str),
    ) -> CoreResult<()> {
        self.temp_seq += 1;
        let path = std::env::temp_dir().join(format!(
            "aweyuki_gui_sideload_{}_{}.zip",
            std::process::id(),
            self.temp_seq
        ));
        std::fs::write(&path, data)?;
        let pstr = path.to_string_lossy().to_string();
        log(&format!("> adb sideload {package_name}"));
        let res = self.run_stream(self.adb_cmd(&["sideload", &pstr]), log);
        let _ = std::fs::remove_file(&path);
        res
    }

    fn wipe_userdata(&mut self, log: &mut dyn FnMut(&str)) -> CoreResult<()> {
        log("> fastboot -w");
        self.run_stream(self.fastboot_cmd(&["-w"]), log)
    }

    fn reboot_system(&mut self, log: &mut dyn FnMut(&str)) -> CoreResult<()> {
        log("> fastboot reboot");
        self.run_stream(self.fastboot_cmd(&["reboot"]), log)
    }
}

fn query_first_real_device() -> Option<DeviceTargetInfo> {
    if let Ok(out) = Command::new("fastboot").arg("devices").output() {
        if out.status.success() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 && (parts[1] == "fastboot" || parts[1] == "fastbootd") {
                    return Some(DeviceTargetInfo {
                        serial: parts[0].to_string(),
                        product: "usb-fastboot".into(),
                        mode: if parts[1] == "fastbootd" {
                            DeviceMode::Fastbootd
                        } else {
                            DeviceMode::Bootloader
                        },
                        current_slot: Some("a".into()),
                        is_ab: true,
                        max_download_size: 256 * 1024 * 1024,
                        existing_logical_partitions: vec![],
                    });
                }
            }
        }
    }
    if let Ok(out) = Command::new("adb").args(["devices"]).output() {
        if out.status.success() {
            for line in String::from_utf8_lossy(&out.stdout).lines().skip(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let mode = if parts[1] == "sideload" {
                        DeviceMode::RecoverySideload
                    } else {
                        DeviceMode::Adb
                    };
                    return Some(DeviceTargetInfo {
                        serial: parts[0].to_string(),
                        product: "usb-adb".into(),
                        mode,
                        current_slot: Some("a".into()),
                        is_ab: true,
                        max_download_size: 256 * 1024 * 1024,
                        existing_logical_partitions: vec![],
                    });
                }
            }
        }
    }
    None
}

pub struct AweyukiApp {
    lang: Lang,
    tab: WorkspaceTab,
    transport_mode: TransportMode,
    sim_profile: SimulatorProfile,
    simulator: DeviceSimulator,
    real_device: Option<DeviceTargetInfo>,

    package: Option<DetectedRomPackage>,
    options: FlashOptions,
    max_size_preset: MaxSizePreset,
    plan: Option<FlashPlan>,

    split_part_idx: usize,
    split_preview: Vec<SparseSplitMeta>,
    split_preview_error: Option<String>,
    script_format: ScriptFormat,

    logs: Vec<String>,
    progress: f32,
    progress_label: String,
    last_report: Option<FlashExecutionReport>,
    status_banner: Option<(bool, String)>,

    bg_rx: Option<mpsc::Receiver<BgMessage>>,
    is_running: bool,
    show_sidebar: bool,
    compact_drawer_open: bool,
}

fn tr<'a>(lang: Lang, vi: &'a str, en: &'a str) -> &'a str {
    match lang {
        Lang::Vi => vi,
        Lang::En => en,
    }
}

impl AweyukiApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_fonts(&cc.egui_ctx);
        setup_theme(&cc.egui_ctx);

        let sim_profile = SimulatorProfile::LowBufferOtg256K;
        let simulator = DeviceSimulator::from_profile(sim_profile);
        let options = FlashOptions {
            max_download_size: 256 * 1024,
            ..FlashOptions::default()
        };

        let mut app = Self {
            lang: Lang::Vi,
            tab: WorkspaceTab::Partitions,
            transport_mode: TransportMode::Simulator,
            sim_profile,
            simulator,
            real_device: query_first_real_device(),
            package: None,
            options,
            max_size_preset: MaxSizePreset::SplitDemo256K,
            plan: None,
            split_part_idx: 0,
            split_preview: Vec::new(),
            split_preview_error: None,
            script_format: ScriptFormat::Bash,
            logs: vec![
                "[Aweyuki] Ready. Loaded built-in Payload OTA sample + 256 KB OTG simulator.".into(),
            ],
            progress: 0.0,
            progress_label: "Idle".into(),
            last_report: None,
            status_banner: None,
            bg_rx: None,
            is_running: false,
            show_sidebar: true,
            compact_drawer_open: false,
        };
        app.load_sample(SampleKind::PayloadOtaZip);
        app
    }

    fn active_device(&self) -> DeviceTargetInfo {
        match self.transport_mode {
            TransportMode::Simulator => self.simulator.info.clone(),
            TransportMode::HardwareUsb => self.real_device.clone().unwrap_or(DeviceTargetInfo {
                serial: "No USB device".into(),
                product: "disconnected".into(),
                mode: DeviceMode::Disconnected,
                current_slot: None,
                is_ab: true,
                max_download_size: 256 * 1024 * 1024,
                existing_logical_partitions: vec![],
            }),
        }
    }

    fn recompute_plan(&mut self) {
        let dev = self.active_device();
        self.options.max_download_size = self.max_size_preset.bytes(dev.max_download_size);
        if let Some(pkg) = &self.package {
            self.plan = Some(build_flash_plan(pkg, &dev, &self.options));
        } else {
            self.plan = None;
        }
    }

    fn recompute_split_preview(&mut self) {
        self.split_preview.clear();
        self.split_preview_error = None;
        let Some(pkg) = &self.package else { return };
        if self.split_part_idx >= pkg.partitions.len() {
            self.split_part_idx = 0;
        }
        let Some(part) = pkg.partitions.get(self.split_part_idx) else {
            return;
        };
        if part.raw_size > 32 * 1024 * 1024 {
            self.split_preview_error = Some(format!(
                "Partition '{}' ({}) > 32 MiB preview cap — actual split runs streaming during flash/CLI.",
                part.partition_name,
                format_bytes_human(part.raw_size)
            ));
            return;
        }
        let dev = self.active_device();
        let max_dl = self.max_size_preset.bytes(dev.max_download_size).max(65_536);
        match part.source.materialize_bytes() {
            Ok(bytes) => match auto_split_image(SourceReader::from_bytes(bytes), max_dl) {
                Ok(blobs) => {
                    self.split_preview = blobs.into_iter().map(|b| b.meta).collect();
                }
                Err(e) => self.split_preview_error = Some(format!("{e}")),
            },
            Err(e) => self.split_preview_error = Some(format!("{e}")),
        }
    }

    fn load_sample(&mut self, kind: SampleKind) {
        match generate_sample_rom(kind) {
            Ok((name, bytes)) => {
                if kind == SampleKind::RecoverySideloadZip {
                    self.sim_profile = SimulatorProfile::RecoverySideloadDevice;
                    self.simulator = DeviceSimulator::from_profile(self.sim_profile);
                    self.transport_mode = TransportMode::Simulator;
                } else if self.sim_profile == SimulatorProfile::RecoverySideloadDevice {
                    self.sim_profile = SimulatorProfile::LowBufferOtg256K;
                    self.simulator = DeviceSimulator::from_profile(self.sim_profile);
                }
                self.load_rom_bytes(name, bytes);
            }
            Err(e) => self.status_banner = Some((false, format!("Sample error: {e}"))),
        }
    }

    fn load_rom_bytes(&mut self, name: String, bytes: Vec<u8>) {
        match detect_from_bytes(name.clone(), bytes) {
            Ok(pkg) => {
                self.logs.push(format!(
                    "[Detect] Loaded '{}' -> {} ({} partition(s))",
                    pkg.package_name,
                    pkg.kind.label(),
                    pkg.partitions.len()
                ));
                let default_idx = pkg
                    .partitions
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, p)| p.raw_size)
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                self.split_part_idx = default_idx;
                self.package = Some(pkg);
                self.status_banner = Some((true, format!("Loaded {name}")));
                self.recompute_plan();
                self.recompute_split_preview();
            }
            Err(e) => {
                self.status_banner = Some((false, format!("Failed to inspect {name}: {e}")));
                self.logs.push(format!("[Error] {name}: {e}"));
            }
        }
    }

    fn load_rom_path(&mut self, path: &std::path::Path) {
        match detect_from_path(path) {
            Ok(pkg) => {
                self.logs.push(format!(
                    "[Detect] Opened '{}' -> {} ({} partition(s))",
                    pkg.package_name,
                    pkg.kind.label(),
                    pkg.partitions.len()
                ));
                let default_idx = pkg
                    .partitions
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, p)| p.raw_size)
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                self.split_part_idx = default_idx;
                self.package = Some(pkg);
                self.status_banner = Some((true, format!("Opened {}", path.display())));
                self.recompute_plan();
                self.recompute_split_preview();
            }
            Err(e) => {
                self.status_banner = Some((false, format!("{e}")));
                self.logs.push(format!("[Error] {}: {e}", path.display()));
            }
        }
    }

    fn export_single_partition(&mut self, idx: usize) {
        let Some(pkg) = &self.package else { return };
        let Some(part) = pkg.partitions.get(idx) else { return };
        let Ok(mut bytes) = part.source.materialize_bytes() else {
            self.status_banner = Some((false, "Extraction failed".into()));
            return;
        };
        if part.is_vbmeta && (self.options.disable_verity || self.options.disable_verification) {
            let _ = patch_vbmeta_verity_flags(
                &mut bytes,
                self.options.disable_verity,
                self.options.disable_verification,
            );
        }
        let default_name = format!("{}.img", part.partition_name);
        if let Some(save_path) = rfd::FileDialog::new()
            .set_file_name(&default_name)
            .save_file()
        {
            match std::fs::write(&save_path, &bytes) {
                Ok(()) => {
                    let msg = format!(
                        "Extracted {} ({}) -> {}",
                        part.partition_name,
                        format_bytes_human(bytes.len() as u64),
                        save_path.display()
                    );
                    self.logs.push(format!("[Unpack] {msg}"));
                    self.status_banner = Some((true, msg));
                }
                Err(e) => self.status_banner = Some((false, format!("Save failed: {e}"))),
            }
        }
    }

    fn start_flash(&mut self, dry_run: bool) {
        let (Some(pkg), Some(plan)) = (self.package.clone(), self.plan.clone()) else {
            return;
        };
        self.tab = WorkspaceTab::FlashConsole;
        self.logs.clear();
        self.progress = 0.0;
        self.last_report = None;

        match self.transport_mode {
            TransportMode::Simulator => {
                let mut sim = self.simulator.clone();
                let mut logs = Vec::new();
                let mut last_prog = (0usize, 1usize, String::new());
                let res = execute_flash_plan(
                    &pkg,
                    &plan,
                    &self.options,
                    &mut sim,
                    dry_run,
                    |cur, tot, msg| {
                        last_prog = (cur, tot.max(1), msg.to_string());
                    },
                    |line| logs.push(line.to_string()),
                );
                self.simulator = sim;
                self.logs.extend(logs);
                self.progress = (last_prog.0 as f32) / (last_prog.1 as f32);
                self.progress_label = last_prog.2;
                match res {
                    Ok(report) => {
                        self.status_banner = Some((
                            true,
                            format!(
                                "{} ({} chunks, {})",
                                if dry_run { "Dry-run OK" } else { "Flash OK" },
                                report.chunks_sent,
                                format_bytes_human(report.total_bytes_transferred)
                            ),
                        ));
                        self.last_report = Some(report);
                    }
                    Err(e) => {
                        self.logs.push(format!("[ERROR] {e}"));
                        self.status_banner = Some((false, format!("{e}")));
                    }
                }
            }
            TransportMode::HardwareUsb => {
                let serial = self.real_device.as_ref().map(|d| d.serial.clone());
                let opts = self.options.clone();
                let (tx, rx) = mpsc::channel();
                self.bg_rx = Some(rx);
                self.is_running = true;
                self.progress_label = "Flashing via USB...".into();

                std::thread::spawn(move || {
                    let mut transport = NativeGuiTransport {
                        serial,
                        temp_seq: 0,
                    };
                    let res = execute_flash_plan(
                        &pkg,
                        &plan,
                        &opts,
                        &mut transport,
                        dry_run,
                        |cur, tot, msg| {
                            let _ = tx.send(BgMessage::Progress(cur, tot, msg.to_string()));
                        },
                        |line| {
                            let _ = tx.send(BgMessage::Log(line.to_string()));
                        },
                    );
                    let _ = tx.send(BgMessage::Finished(res.map_err(|e| e.to_string())));
                });
            }
        }
    }

    fn poll_external_events(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            if let Some(bytes) = file.bytes {
                let name = if file.name.is_empty() {
                    "dropped_rom.bin".to_string()
                } else {
                    file.name
                };
                self.load_rom_bytes(name, bytes.to_vec());
                break;
            }
            if let Some(path) = file.path {
                self.load_rom_path(&path);
                break;
            }
        }

        if let Some(rx) = &self.bg_rx {
            let mut finished = None;
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    BgMessage::Log(l) => self.logs.push(l),
                    BgMessage::Progress(cur, tot, label) => {
                        self.progress = (cur as f32) / (tot.max(1) as f32);
                        self.progress_label = label;
                    }
                    BgMessage::Finished(r) => finished = Some(r),
                }
            }
            if let Some(res) = finished {
                self.is_running = false;
                self.bg_rx = None;
                self.progress = 1.0;
                match res {
                    Ok(rep) => {
                        self.status_banner = Some((
                            true,
                            format!(
                                "USB Flash OK ({} chunks, {})",
                                rep.chunks_sent,
                                format_bytes_human(rep.total_bytes_transferred)
                            ),
                        ));
                        self.last_report = Some(rep);
                    }
                    Err(err) => {
                        self.logs.push(format!("[ERROR] {err}"));
                        self.status_banner = Some((false, err));
                    }
                }
            } else if self.is_running {
                ctx.request_repaint();
            }
        }
    }

    fn render_device_and_options(
        &mut self,
        ui: &mut egui::Ui,
        lang: Lang,
        need_replan: &mut bool,
        need_resplit: &mut bool,
    ) {
        obsidian_card(
            ui,
            tr(lang, "THIẾT BỊ ĐÍCH", "TARGET DEVICE"),
            |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(
                            self.transport_mode == TransportMode::HardwareUsb,
                            "USB Hardware",
                        )
                        .clicked()
                    {
                        self.transport_mode = TransportMode::HardwareUsb;
                        self.real_device = query_first_real_device();
                        *need_replan = true;
                    }
                    if ui
                        .selectable_label(
                            self.transport_mode == TransportMode::Simulator,
                            tr(lang, "Giả lập ảo", "Simulator"),
                        )
                        .clicked()
                    {
                        self.transport_mode = TransportMode::Simulator;
                        *need_replan = true;
                    }
                });
                ui.add_space(6.0);

                let dev = self.active_device();
                egui::Frame::NONE
                    .fill(BG_SURFACE)
                    .corner_radius(CornerRadius::same(8))
                    .stroke(Stroke::new(1.0_f32, BORDER_HAIR))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let (bg, fg) = match dev.mode {
                                DeviceMode::Bootloader => (
                                    Color32::from_rgb(16, 60, 38),
                                    Color32::from_rgb(134, 239, 172),
                                ),
                                DeviceMode::Fastbootd => (
                                    Color32::from_rgb(65, 25, 98),
                                    Color32::from_rgb(216, 180, 254),
                                ),
                                DeviceMode::RecoverySideload => (
                                    Color32::from_rgb(88, 42, 12),
                                    Color32::from_rgb(253, 224, 71),
                                ),
                                DeviceMode::Adb => (
                                    Color32::from_rgb(20, 48, 92),
                                    Color32::from_rgb(147, 197, 253),
                                ),
                                DeviceMode::Disconnected => (
                                    Color32::from_rgb(88, 22, 22),
                                    Color32::from_rgb(252, 165, 165),
                                ),
                            };
                            pill(ui, dev.mode.label(), bg, fg);
                            ui.label(RichText::new(&dev.product).strong().color(TEXT_PRIMARY));
                        });
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(format!(
                                "{} • Slot {} • {}",
                                dev.serial,
                                dev.current_slot.as_deref().unwrap_or("n/a"),
                                format_bytes_human(dev.max_download_size)
                            ))
                            .small()
                            .monospace()
                            .color(TEXT_MUTED),
                        );
                    });

                ui.add_space(8.0);
                if self.transport_mode == TransportMode::Simulator {
                    egui::ComboBox::from_id_salt("sim_profile")
                        .width(ui.available_width())
                        .selected_text(self.sim_profile.label())
                        .show_ui(ui, |ui| {
                            for prof in SimulatorProfile::all() {
                                if ui
                                    .selectable_value(&mut self.sim_profile, *prof, prof.label())
                                    .clicked()
                                {
                                    self.simulator = DeviceSimulator::from_profile(*prof);
                                    if *prof == SimulatorProfile::LowBufferOtg256K {
                                        self.max_size_preset = MaxSizePreset::SplitDemo256K;
                                    }
                                    *need_replan = true;
                                    *need_resplit = true;
                                }
                            }
                        });
                } else if ui
                    .button(tr(lang, "Quét lại USB / Termux", "Refresh USB / Termux"))
                    .clicked()
                {
                    self.real_device = query_first_real_device();
                    *need_replan = true;
                }
            },
        );

        ui.add_space(12.0);

        obsidian_card(
            ui,
            tr(lang, "CHIA NHỎ & CẤU HÌNH FLASH", "SPLIT & FLASH OPTIONS"),
            |ui| {
                ui.label(
                    RichText::new(tr(
                        lang,
                        "Giới hạn chia chunk (max-download-size)",
                        "Chunk Limit (max-download-size)",
                    ))
                    .small()
                    .color(TEXT_MUTED),
                );
                egui::ComboBox::from_id_salt("max_dl_preset")
                    .width(ui.available_width())
                    .selected_text(self.max_size_preset.label(lang))
                    .show_ui(ui, |ui| {
                        for p in MaxSizePreset::all() {
                            if ui
                                .selectable_value(&mut self.max_size_preset, *p, p.label(lang))
                                .clicked()
                            {
                                *need_replan = true;
                                *need_resplit = true;
                            }
                        }
                    });

                ui.add_space(6.0);
                ui.label(RichText::new(tr(lang, "Slot đích", "Target Slot")).small().color(TEXT_MUTED));
                egui::ComboBox::from_id_salt("slot_sel")
                    .width(ui.available_width())
                    .selected_text(self.options.slot.label())
                    .show_ui(ui, |ui| {
                        for s in [
                            SlotSelection::Active,
                            SlotSelection::SlotA,
                            SlotSelection::SlotB,
                            SlotSelection::BothSlots,
                        ] {
                            if ui
                                .selectable_value(&mut self.options.slot, s, s.label())
                                .clicked()
                            {
                                *need_replan = true;
                            }
                        }
                    });

                ui.add_space(6.0);
                ui.label(
                    RichText::new(tr(lang, "Chiến lược super.img", "super.img Strategy"))
                        .small()
                        .color(TEXT_MUTED),
                );
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(
                            self.options.super_mode == SuperFlashMode::WholeSparse,
                            tr(lang, "Nguyên khối", "Whole Sparse"),
                        )
                        .clicked()
                    {
                        self.options.super_mode = SuperFlashMode::WholeSparse;
                        *need_replan = true;
                    }
                    if ui
                        .selectable_label(
                            self.options.super_mode == SuperFlashMode::UnpackToFastbootd,
                            tr(lang, "Bung fastbootd", "Unpack fastbootd"),
                        )
                        .clicked()
                    {
                        self.options.super_mode = SuperFlashMode::UnpackToFastbootd;
                        *need_replan = true;
                    }
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);

                if toggle_switch_row(
                    ui,
                    &mut self.options.auto_sparse_split,
                    tr(lang, "Tự cắt Sparse (.simg chunks)", "Auto Sparse Split"),
                ) {
                    *need_replan = true;
                }
                if toggle_switch_row(
                    ui,
                    &mut self.options.disable_verity,
                    tr(lang, "Patch vbmeta (--disable-verity)", "Patch vbmeta (AVB0)"),
                ) {
                    self.options.disable_verification = self.options.disable_verity;
                    *need_replan = true;
                }
                if toggle_switch_row(
                    ui,
                    &mut self.options.auto_resize_logical,
                    tr(lang, "Resize logical & xoá -cow", "Auto resize & drop -cow"),
                ) {
                    self.options.reclaim_cow = self.options.auto_resize_logical;
                    *need_replan = true;
                }
                if toggle_switch_row(
                    ui,
                    &mut self.options.force_sideload,
                    tr(lang, "Ưu tiên ADB Sideload", "Force ADB Sideload"),
                ) {
                    *need_replan = true;
                }
                if toggle_switch_row(
                    ui,
                    &mut self.options.wipe_userdata,
                    tr(lang, "Xoá dữ liệu (Wipe -w)", "Wipe Userdata (-w)"),
                ) {
                    *need_replan = true;
                }
                if toggle_switch_row(
                    ui,
                    &mut self.options.reboot_after,
                    tr(lang, "Tự khởi động lại", "Reboot when done"),
                ) {
                    *need_replan = true;
                }
            },
        );

        ui.add_space(12.0);
        let can_run = self.package.is_some() && !self.is_running;
        ui.add_enabled_ui(can_run, |ui| {
            let flash_btn = egui::Button::new(
                RichText::new(tr(lang, "⚡ Bắt đầu Flash ngay", "⚡ Start Flash Now"))
                    .strong()
                    .size(14.0)
                    .color(BG_CANVAS),
            )
            .fill(ACCENT_CYAN)
            .corner_radius(CornerRadius::same(8))
            .min_size(Vec2::new(ui.available_width(), 38.0));
            if ui.add(flash_btn).clicked() {
                self.start_flash(false);
            }
            ui.add_space(6.0);
            let dry_btn = egui::Button::new(
                RichText::new(tr(lang, "▶ Chạy thử (Dry-Run)", "▶ Dry-Run Check"))
                    .color(TEXT_PRIMARY),
            )
            .fill(BG_SURFACE)
            .stroke(Stroke::new(1.0_f32, BORDER_HAIR))
            .corner_radius(CornerRadius::same(8))
            .min_size(Vec2::new(ui.available_width(), 32.0));
            if ui.add(dry_btn).clicked() {
                self.start_flash(true);
            }
        });
    }
}

impl eframe::App for AweyukiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_external_events(ctx);

        let lang = self.lang;
        let is_narrow = ctx.screen_rect().width() < 860.0;
        let mut need_replan = false;
        let mut need_resplit = false;
        let mut sample_to_load: Option<SampleKind> = None;

        // Clean Single-Row 54px Top Header
        egui::TopBottomPanel::top("top_bar")
            .frame(
                egui::Frame::NONE
                    .fill(BG_CHROME)
                    .inner_margin(egui::Margin::symmetric(18, 10))
                    .stroke(Stroke::new(1.0_f32, BORDER_HAIR)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("AWEYUKI")
                            .size(16.0)
                            .strong()
                            .color(ACCENT_CYAN),
                    );
                    ui.label(
                        RichText::new("Flasher Studio")
                            .size(14.0)
                            .color(TEXT_MUTED),
                    );
                    pill(
                        ui,
                        "Native wgpu",
                        Color32::from_rgb(30, 38, 68),
                        Color32::from_rgb(165, 180, 252),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let lang_btn = if self.lang == Lang::Vi { "VN" } else { "EN" };
                        if ui.button(lang_btn).clicked() {
                            self.lang = if self.lang == Lang::Vi {
                                Lang::En
                            } else {
                                Lang::Vi
                            };
                        }
                        let cfg_lbl = tr(lang, "Cấu hình", "Config");
                        if is_narrow {
                            ui.toggle_value(&mut self.compact_drawer_open, cfg_lbl);
                        } else {
                            ui.toggle_value(&mut self.show_sidebar, cfg_lbl);
                        }

                        ui.menu_button(tr(lang, "ROM Mẫu ▼", "Samples ▼"), |ui| {
                            for (lbl, kind) in [
                                ("Payload OTA (.zip)", SampleKind::PayloadOtaZip),
                                ("payload.bin (CrAU)", SampleKind::StandalonePayload),
                                ("super.img (Sparse LP)", SampleKind::SuperSparse),
                                ("Fastboot ROM (.zip)", SampleKind::FastbootZip),
                                ("Recovery Sideload (.zip)", SampleKind::RecoverySideloadZip),
                            ] {
                                if ui.button(lbl).clicked() {
                                    sample_to_load = Some(kind);
                                    ui.close_menu();
                                }
                            }
                        });

                        if ui
                            .button(tr(lang, "Thư mục...", "Folder..."))
                            .clicked()
                        {
                            if let Some(p) = rfd::FileDialog::new().pick_folder() {
                                self.load_rom_path(&p);
                            }
                        }

                        if ui
                            .button(tr(lang, "+ Mở File ROM", "+ Open ROM"))
                            .clicked()
                        {
                            if let Some(p) = rfd::FileDialog::new()
                                .add_filter("ROM Files", &["zip", "bin", "img"])
                                .pick_file()
                            {
                                self.load_rom_path(&p);
                            }
                        }
                    });
                });
            });

        if let Some(k) = sample_to_load {
            self.load_sample(k);
        }

        if !is_narrow && self.show_sidebar {
            egui::SidePanel::left("sidebar")
                .resizable(true)
                .default_width(305.0)
                .min_width(275.0)
                .frame(
                    egui::Frame::NONE
                        .fill(BG_CHROME)
                        .inner_margin(egui::Margin::same(14))
                        .stroke(Stroke::new(1.0_f32, BORDER_HAIR)),
                )
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        self.render_device_and_options(
                            ui,
                            lang,
                            &mut need_replan,
                            &mut need_resplit,
                        );
                    });
                });
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(BG_CANVAS)
                    .inner_margin(egui::Margin::same(18)),
            )
            .show(ctx, |ui| {
                if is_narrow {
                    let dev = self.active_device();
                    let can_run = self.package.is_some() && !self.is_running;
                    ui.horizontal_wrapped(|ui| {
                        pill(
                            ui,
                            &format!("{} • {}", dev.mode.label(), dev.product),
                            Color32::from_rgb(16, 60, 38),
                            Color32::from_rgb(134, 239, 172),
                        );
                        ui.add_enabled_ui(can_run, |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(tr(lang, "⚡ Flash ngay", "⚡ Flash"))
                                            .strong()
                                            .color(BG_CANVAS),
                                    )
                                    .fill(ACCENT_CYAN),
                                )
                                .clicked()
                            {
                                self.start_flash(false);
                            }
                            if ui.button("▶ Dry-Run").clicked() {
                                self.start_flash(true);
                            }
                        });
                    });
                    if self.compact_drawer_open {
                        ui.add_space(6.0);
                        egui::ScrollArea::vertical()
                            .max_height(280.0)
                            .show(ui, |ui| {
                                self.render_device_and_options(
                                    ui,
                                    lang,
                                    &mut need_replan,
                                    &mut need_resplit,
                                );
                            });
                    }
                    ui.add_space(8.0);
                }

                if let Some((ok, msg)) = &self.status_banner {
                    let (bg, fg) = if *ok {
                        (Color32::from_rgb(15, 42, 31), Color32::from_rgb(110, 231, 183))
                    } else {
                        (Color32::from_rgb(60, 18, 22), Color32::from_rgb(252, 165, 165))
                    };
                    egui::Frame::NONE
                        .fill(bg)
                        .corner_radius(CornerRadius::same(8))
                        .stroke(Stroke::new(1.0_f32, fg.linear_multiply(0.35)))
                        .inner_margin(egui::Margin::symmetric(12, 7))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(if *ok { "✓" } else { "✕" }).color(fg).strong());
                                ui.label(RichText::new(msg).color(fg));
                            });
                        });
                    ui.add_space(10.0);
                }

                // Underline Tab Bar
                ui.horizontal_wrapped(|ui| {
                    for (tab, vi, en) in [
                        (
                            WorkspaceTab::Partitions,
                            "Phân vùng & Kế hoạch",
                            "Partitions & Plan",
                        ),
                        (
                            WorkspaceTab::ChunkSplit,
                            "Chia nhỏ Phân vùng",
                            "Chunk & Super Splitter",
                        ),
                        (
                            WorkspaceTab::ExportScript,
                            "Xuất Script",
                            "Export Script",
                        ),
                        (
                            WorkspaceTab::FlashConsole,
                            "Console",
                            "Console",
                        ),
                    ] {
                        if tab_underline_btn(ui, self.tab == tab, tr(lang, vi, en)) {
                            self.tab = tab;
                        }
                    }
                });
                ui.add_space(12.0);

                match self.tab {
                    WorkspaceTab::Partitions => {
                        let mut unpack_idx: Option<usize> = None;
                        let mut inspect_split_idx: Option<usize> = None;
                        let mut toggle_changed = false;
                        let auto_sparse_split = self.options.auto_sparse_split;
                        let max_dl = self.options.max_download_size.max(65_536);

                        if let Some(pkg) = &mut self.package {
                            let plan_ref = &self.plan;
                            egui::ScrollArea::vertical().show(ui, |ui| {
                                // Hero + Pipeline Card
                                egui::Frame::NONE
                                    .fill(BG_CARD)
                                    .corner_radius(CornerRadius::same(10))
                                    .stroke(Stroke::new(1.0_f32, BORDER_HAIR))
                                    .inner_margin(egui::Margin::same(14))
                                    .show(ui, |ui| {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(
                                                RichText::new(&pkg.package_name)
                                                    .size(16.0)
                                                    .strong()
                                                    .color(TEXT_PRIMARY),
                                            );
                                            pill(
                                                ui,
                                                pkg.kind.label(),
                                                Color32::from_rgb(17, 45, 70),
                                                ACCENT_CYAN,
                                            );
                                            pill(
                                                ui,
                                                &format_bytes_human(pkg.total_input_bytes),
                                                BG_SURFACE,
                                                TEXT_MUTED,
                                            );
                                            if let Some(c) = &pkg.codename_hint {
                                                pill(
                                                    ui,
                                                    &format!("Board: {c}"),
                                                    Color32::from_rgb(16, 54, 34),
                                                    Color32::from_rgb(134, 239, 172),
                                                );
                                            }
                                            if pkg.kind == RomPackageKind::FastbootZip
                                                && !pkg.stripped_lock_commands.is_empty()
                                            {
                                                pill(
                                                    ui,
                                                    &format!(
                                                        "Blocked {} oem lock",
                                                        pkg.stripped_lock_commands.len()
                                                    ),
                                                    Color32::from_rgb(66, 40, 12),
                                                    Color32::from_rgb(253, 224, 71),
                                                );
                                            }
                                        });
                                        for w in &pkg.warnings {
                                            ui.add_space(4.0);
                                            ui.label(
                                                RichText::new(format!("⚠ {w}"))
                                                    .small()
                                                    .color(ACCENT_AMBER),
                                            );
                                        }
                                        if let Some(plan) = plan_ref {
                                            ui.add_space(10.0);
                                            render_pipeline_stepper(ui, plan, lang);
                                        }
                                    });

                                ui.add_space(12.0);
                                obsidian_card(
                                    ui,
                                    tr(
                                        lang,
                                        "PHÂN VÙNG TỰ ĐỘNG PHÁT HIỆN (HỖ TRỢ KÉO-THẢ ROM)",
                                        "AUTO-DETECTED PARTITIONS (DRAG & DROP SUPPORTED)",
                                    ),
                                    |ui| {
                                        egui::ScrollArea::horizontal()
                                            .id_salt("part_grid_h")
                                            .show(ui, |ui| {
                                                egui::Grid::new("part_grid")
                                                    .striped(true)
                                                    .spacing([14.0, 8.0])
                                                    .show(ui, |ui| {
                                                        ui.label(RichText::new("Flash").small().color(TEXT_DIM));
                                                        ui.label(RichText::new("Partition").small().color(TEXT_DIM));
                                                        ui.label(RichText::new("Format").small().color(TEXT_DIM));
                                                        ui.label(RichText::new("Size").small().color(TEXT_DIM));
                                                        ui.label(RichText::new("Stage").small().color(TEXT_DIM));
                                                        ui.label(RichText::new("Chunks").small().color(TEXT_DIM));
                                                        ui.label(RichText::new("Actions").small().color(TEXT_DIM));
                                                        ui.end_row();

                                                        for (idx, p) in pkg.partitions.iter_mut().enumerate() {
                                                            if toggle_pill(ui, &mut p.selected) {
                                                                toggle_changed = true;
                                                            }
                                                            ui.label(
                                                                RichText::new(&p.partition_name)
                                                                    .strong()
                                                                    .monospace()
                                                                    .color(TEXT_PRIMARY),
                                                            );
                                                            pill(
                                                                ui,
                                                                p.format.label(),
                                                                BG_SURFACE,
                                                                ACCENT_CYAN,
                                                            );
                                                            ui.label(
                                                                RichText::new(format_bytes_human(p.raw_size))
                                                                    .monospace(),
                                                            );
                                                            let (st_lbl, st_bg, st_fg) = match p.target_mode {
                                                                PartitionTargetMode::Bootloader => (
                                                                    "bootloader",
                                                                    Color32::from_rgb(16, 54, 34),
                                                                    Color32::from_rgb(134, 239, 172),
                                                                ),
                                                                PartitionTargetMode::FastbootdLogical => (
                                                                    "fastbootd",
                                                                    Color32::from_rgb(54, 24, 78),
                                                                    Color32::from_rgb(216, 180, 254),
                                                                ),
                                                                PartitionTargetMode::SideloadOnly => (
                                                                    "sideload",
                                                                    Color32::from_rgb(66, 40, 12),
                                                                    Color32::from_rgb(253, 224, 71),
                                                                ),
                                                            };
                                                            pill(ui, st_lbl, st_bg, st_fg);

                                                            let sz_eff = p.sparse_size.unwrap_or(p.raw_size);
                                                            let est_chunks = if auto_sparse_split && sz_eff > max_dl {
                                                                sz_eff.div_ceil(max_dl.saturating_sub(128).max(4096))
                                                            } else {
                                                                1
                                                            };
                                                            if est_chunks > 1 {
                                                                pill(
                                                                    ui,
                                                                    &format!("{est_chunks} chunks"),
                                                                    Color32::from_rgb(66, 40, 12),
                                                                    Color32::from_rgb(253, 224, 71),
                                                                );
                                                            } else {
                                                                pill(
                                                                    ui,
                                                                    "1",
                                                                    Color32::from_rgb(16, 54, 34),
                                                                    Color32::from_rgb(134, 239, 172),
                                                                );
                                                            }

                                                            ui.horizontal(|ui| {
                                                                if p.format != DetectedFormat::SideloadPackage
                                                                    && ui.small_button("Unpack .img").clicked()
                                                                {
                                                                    unpack_idx = Some(idx);
                                                                }
                                                                if ui.small_button("Split View").clicked() {
                                                                    inspect_split_idx = Some(idx);
                                                                }
                                                            });
                                                            ui.end_row();
                                                        }
                                                    });
                                            });
                                    },
                                );

                                ui.add_space(12.0);
                                if let Some(plan) = plan_ref {
                                    obsidian_card(
                                        ui,
                                        &format!(
                                            "{} ({})",
                                            tr(lang, "TRÌNH TỰ LỆNH FLASH", "EXECUTION PLAN"),
                                            plan.steps.len()
                                        ),
                                        |ui| {
                                            for (i, st) in plan.steps.iter().enumerate() {
                                                let col = match st.stage() {
                                                    FlashStage::Bootloader => ACCENT_GREEN,
                                                    FlashStage::Transition => ACCENT_AMBER,
                                                    FlashStage::Fastbootd => ACCENT_VIOLET,
                                                    FlashStage::Sideload => ACCENT_CYAN,
                                                    FlashStage::Finalize => TEXT_MUTED,
                                                };
                                                ui.label(
                                                    RichText::new(format!("{:>2}. {}", i + 1, st.summary()))
                                                        .monospace()
                                                        .color(col),
                                                );
                                            }
                                        },
                                    );
                                }
                            });
                        }

                        if toggle_changed {
                            need_replan = true;
                        }
                        if let Some(idx) = unpack_idx {
                            self.export_single_partition(idx);
                        }
                        if let Some(idx) = inspect_split_idx {
                            self.split_part_idx = idx;
                            self.tab = WorkspaceTab::ChunkSplit;
                            need_resplit = true;
                        }
                    }

                    WorkspaceTab::ChunkSplit => {
                        let part_choices: Vec<(usize, String)> = self
                            .package
                            .as_ref()
                            .map(|pkg| {
                                pkg.partitions
                                    .iter()
                                    .enumerate()
                                    .map(|(i, p)| {
                                        (
                                            i,
                                            format!(
                                                "{} ({})",
                                                p.partition_name,
                                                format_bytes_human(p.raw_size)
                                            ),
                                        )
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        let cur_name = self
                            .package
                            .as_ref()
                            .and_then(|pkg| pkg.partitions.get(self.split_part_idx))
                            .map(|p| p.partition_name.clone())
                            .unwrap_or_default();
                        let max_dl_opt = self.options.max_download_size;

                        egui::ScrollArea::vertical().show(ui, |ui| {
                            if !part_choices.is_empty() {
                                obsidian_card(
                                    ui,
                                    tr(
                                        lang,
                                        "TRÌNH MÔ PHỎNG CẮT NHỎ SPARSE CHUNK",
                                        "SPARSE CHUNK SPLIT VISUALIZER",
                                    ),
                                    |ui| {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.label(tr(lang, "Phân vùng:", "Partition:"));
                                            egui::ComboBox::from_id_salt("split_part_combo")
                                                .selected_text(&cur_name)
                                                .show_ui(ui, |ui| {
                                                    for (i, label) in &part_choices {
                                                        if ui
                                                            .selectable_value(
                                                                &mut self.split_part_idx,
                                                                *i,
                                                                label,
                                                            )
                                                            .clicked()
                                                        {
                                                            need_resplit = true;
                                                        }
                                                    }
                                                });
                                            pill(
                                                ui,
                                                &format!("max-dl: {}", format_bytes_human(max_dl_opt)),
                                                BG_SURFACE,
                                                ACCENT_CYAN,
                                            );
                                            pill(ui, "DONT_CARE", Color32::from_rgb(35, 44, 59), TEXT_MUTED);
                                            pill(ui, "RAW", Color32::from_rgb(14, 58, 92), ACCENT_CYAN);
                                            pill(ui, "FILL(0)", Color32::from_rgb(16, 54, 34), ACCENT_GREEN);
                                        });
                                        ui.add_space(10.0);

                                        if let Some(err) = &self.split_preview_error {
                                            ui.label(RichText::new(err).color(ACCENT_AMBER));
                                        } else {
                                            for meta in &self.split_preview {
                                                ui.label(
                                                    RichText::new(format!(
                                                        "Chunk {}/{} • {} (.simg) • blocks {}..{}/{}",
                                                        meta.index + 1,
                                                        meta.total_parts,
                                                        format_bytes_human(meta.encoded_bytes as u64),
                                                        meta.start_block,
                                                        meta.start_block + meta.active_blocks,
                                                        meta.total_blocks
                                                    ))
                                                    .monospace()
                                                    .small(),
                                                );
                                                let desired = Vec2::new(ui.available_width(), 16.0);
                                                let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());
                                                let painter = ui.painter_at(rect);
                                                painter.rect_filled(rect, CornerRadius::same(5), BG_CANVAS);
                                                let tot = meta.total_blocks.max(1) as f32;
                                                for seg in &meta.segments {
                                                    let x0 = rect.left() + (seg.start_block as f32 / tot) * rect.width();
                                                    let x1 = rect.left() + ((seg.start_block + seg.block_count) as f32 / tot) * rect.width();
                                                    let color = match seg.kind {
                                                        VisualSegmentKind::DontCareLead | VisualSegmentKind::DontCareTail => {
                                                            Color32::from_rgb(51, 65, 85)
                                                        }
                                                        VisualSegmentKind::Raw => ACCENT_CYAN,
                                                        VisualSegmentKind::FillZero => ACCENT_GREEN,
                                                        VisualSegmentKind::DontCareMiddle => ACCENT_VIOLET,
                                                    };
                                                    let seg_rect = egui::Rect::from_min_max(
                                                        egui::pos2(x0, rect.top() + 2.0),
                                                        egui::pos2(x1.max(x0 + 2.0), rect.bottom() - 2.0),
                                                    );
                                                    painter.rect_filled(seg_rect, CornerRadius::same(2), color);
                                                }
                                                ui.add_space(6.0);
                                            }
                                        }
                                    },
                                );

                                if let Some(si) = self.package.as_ref().and_then(|p| p.super_info.as_ref()) {
                                    ui.add_space(12.0);
                                    obsidian_card(ui, "DYNAMIC SUPER LAYOUT (LPMETADATA)", |ui| {
                                        for g in &si.groups {
                                            if g.name == "default" {
                                                continue;
                                            }
                                            let pct = if g.maximum_size > 0 {
                                                (g.used_bytes as f32 / g.maximum_size as f32).clamp(0.0, 1.0)
                                            } else {
                                                0.0
                                            };
                                            ui.label(format!(
                                                "Group '{}': {} / {}",
                                                g.name,
                                                format_bytes_human(g.used_bytes),
                                                format_bytes_human(g.maximum_size)
                                            ));
                                            ui.add(egui::ProgressBar::new(pct).show_percentage());
                                        }
                                        ui.add_space(8.0);
                                        for lp in &si.partitions {
                                            ui.label(
                                                RichText::new(format!(
                                                    "  ● {:<16} {} [{}]",
                                                    lp.name,
                                                    format_bytes_human(lp.size_bytes),
                                                    lp.group_name
                                                ))
                                                .monospace(),
                                            );
                                        }
                                    });
                                }
                            }
                        });
                    }

                    WorkspaceTab::ExportScript => {
                        ui.horizontal_wrapped(|ui| {
                            for (fmt, lbl) in [
                                (ScriptFormat::Bash, "Linux / macOS (.sh)"),
                                (ScriptFormat::WindowsBatch, "Windows (.bat)"),
                                (ScriptFormat::TermuxOtg, "Termux Android OTG (.sh)"),
                            ] {
                                ui.selectable_value(&mut self.script_format, fmt, lbl);
                            }
                        });
                        ui.add_space(8.0);
                        if let Some(plan) = &self.plan {
                            let mut script = export_script(plan, self.script_format);
                            let fname = match self.script_format {
                                ScriptFormat::Bash => "flash_rom.sh",
                                ScriptFormat::WindowsBatch => "flash_rom.bat",
                                ScriptFormat::TermuxOtg => "flash_termux_otg.sh",
                            };
                            if ui.button(tr(lang, "Lưu Script...", "Save Script...")).clicked() {
                                if let Some(p) = rfd::FileDialog::new().set_file_name(fname).save_file() {
                                    let _ = std::fs::write(&p, script.as_bytes());
                                    self.status_banner = Some((true, format!("Saved {}", p.display())));
                                }
                            }
                            ui.add_space(6.0);
                            egui::ScrollArea::both().show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut script)
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(f32::INFINITY)
                                        .desired_rows(22),
                                );
                            });
                        }
                    }

                    WorkspaceTab::FlashConsole => {
                        obsidian_card(ui, tr(lang, "TIẾN TRÌNH THỰC THI", "EXECUTION PROGRESS"), |ui| {
                            ui.add(
                                egui::ProgressBar::new(self.progress)
                                    .text(&self.progress_label)
                                    .animate(self.is_running),
                            );
                            if let Some(rep) = &self.last_report {
                                ui.add_space(8.0);
                                ui.horizontal_wrapped(|ui| {
                                    pill(
                                        ui,
                                        &format!("Steps: {}", rep.steps_completed),
                                        Color32::from_rgb(16, 54, 34),
                                        ACCENT_GREEN,
                                    );
                                    pill(
                                        ui,
                                        &format!("Chunks sent: {}", rep.chunks_sent),
                                        Color32::from_rgb(14, 58, 92),
                                        ACCENT_CYAN,
                                    );
                                    pill(
                                        ui,
                                        &format!("Transferred: {}", format_bytes_human(rep.total_bytes_transferred)),
                                        BG_SURFACE,
                                        TEXT_PRIMARY,
                                    );
                                });
                            }
                        });
                        ui.add_space(10.0);
                        obsidian_card(ui, "FASTBOOT / ADB CONSOLE", |ui| {
                            egui::ScrollArea::vertical()
                                .stick_to_bottom(true)
                                .show(ui, |ui| {
                                    for line in &self.logs {
                                        let color = if line.contains("ERROR") || line.contains("Failed") {
                                            Color32::from_rgb(248, 113, 113)
                                        } else if line.contains("OKAY") || line.contains("Finished") || line.contains("patched") {
                                            ACCENT_GREEN
                                        } else if line.contains("Split") || line.contains("Chunk") {
                                            ACCENT_CYAN
                                        } else {
                                            TEXT_MUTED
                                        };
                                        ui.label(RichText::new(line).monospace().color(color));
                                    }
                                });
                        });
                    }
                }
            });

        if need_replan {
            self.recompute_plan();
        }
        if need_resplit {
            self.recompute_split_preview();
        }
    }
}

fn toggle_pill(ui: &mut egui::Ui, on: &mut bool) -> bool {
    let desired = Vec2::new(34.0, 18.0);
    let (rect, mut resp) = ui.allocate_exact_size(desired, egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let how_on = ui.ctx().animate_bool_responsive(resp.id, *on);
    let bg = if *on {
        Color32::from_rgb(20, 74, 102)
    } else {
        BG_SURFACE
    };
    let border = if *on { ACCENT_CYAN } else { BORDER_HAIR };
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(9),
        bg,
        Stroke::new(1.0_f32, border),
        egui::StrokeKind::Inside,
    );
    let circle_x = egui::lerp((rect.left() + 9.0)..=(rect.right() - 9.0), how_on);
    let circle_color = if *on { ACCENT_CYAN } else { TEXT_MUTED };
    painter.circle_filled(egui::pos2(circle_x, rect.center().y), 6.0, circle_color);
    resp.changed()
}

fn toggle_switch_row(ui: &mut egui::Ui, value: &mut bool, label: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(TEXT_PRIMARY).size(12.5));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            changed = toggle_pill(ui, value);
        });
    });
    ui.add_space(3.0);
    changed
}

fn tab_underline_btn(ui: &mut egui::Ui, active: bool, label: &str) -> bool {
    let text = RichText::new(label)
        .size(13.5)
        .strong()
        .color(if active { ACCENT_CYAN } else { TEXT_MUTED });
    let resp = ui.add(
        egui::Button::new(text)
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(4)),
    );
    if active {
        let r = resp.rect;
        ui.painter().line_segment(
            [
                egui::pos2(r.left() + 4.0, r.bottom()),
                egui::pos2(r.right() - 4.0, r.bottom()),
            ],
            Stroke::new(2.0_f32, ACCENT_CYAN),
        );
    }
    resp.clicked()
}

fn render_pipeline_stepper(ui: &mut egui::Ui, plan: &FlashPlan, lang: Lang) {
    let mut boot_cnt = 0usize;
    let mut fb_cnt = 0usize;
    let mut side_cnt = 0usize;
    let mut fin_cnt = 0usize;
    for s in &plan.steps {
        match s.stage() {
            FlashStage::Bootloader => boot_cnt += 1,
            FlashStage::Transition | FlashStage::Fastbootd => fb_cnt += 1,
            FlashStage::Sideload => side_cnt += 1,
            FlashStage::Finalize => fin_cnt += 1,
        }
    }
    let suffix = tr(lang, "bước", "steps");
    ui.horizontal_wrapped(|ui| {
        for (idx, (num, title, cnt)) in [
            ("01", "Bootloader", boot_cnt),
            ("02", "Fastbootd", fb_cnt),
            ("03", "ADB Sideload", side_cnt),
            ("04", tr(lang, "Hoàn tất", "Finalize"), fin_cnt),
        ]
        .into_iter()
        .enumerate()
        {
            let active = cnt > 0;
            egui::Frame::NONE
                .fill(if active {
                    Color32::from_rgb(16, 32, 48)
                } else {
                    BG_SURFACE
                })
                .corner_radius(CornerRadius::same(8))
                .stroke(Stroke::new(
                    1.0_f32,
                    if active {
                        Color32::from_rgb(28, 78, 108)
                    } else {
                        BORDER_HAIR
                    },
                ))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(num)
                                .monospace()
                                .strong()
                                .small()
                                .color(if active { ACCENT_CYAN } else { TEXT_DIM }),
                        );
                        ui.label(
                            RichText::new(format!("{title} ({cnt} {suffix})"))
                                .small()
                                .color(if active { TEXT_PRIMARY } else { TEXT_DIM }),
                        );
                    });
                });
            if idx < 3 {
                ui.label(RichText::new("→").color(TEXT_DIM));
            }
        }
    });
}

fn pill(ui: &mut egui::Ui, text: &str, bg: Color32, fg: Color32) {
    egui::Frame::NONE
        .fill(bg)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().strong().color(fg));
        });
}

fn obsidian_card(ui: &mut egui::Ui, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .fill(BG_CARD)
        .corner_radius(CornerRadius::same(10))
        .stroke(Stroke::new(1.0_f32, BORDER_HAIR))
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.label(
                RichText::new(title)
                    .small()
                    .strong()
                    .color(TEXT_MUTED),
            );
            ui.add_space(8.0);
            add_contents(ui);
        });
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "inter".to_owned(),
        FontData::from_static(FONT_INTER).into(),
    );
    fonts.font_data.insert(
        "dejavu_sans".to_owned(),
        FontData::from_static(FONT_DEJAVU).into(),
    );
    fonts.font_data.insert(
        "jetbrains_mono".to_owned(),
        FontData::from_static(FONT_MONO).into(),
    );
    if let Some(vec) = fonts.families.get_mut(&FontFamily::Proportional) {
        vec.insert(0, "inter".to_owned());
        vec.insert(1, "dejavu_sans".to_owned());
    }
    if let Some(vec) = fonts.families.get_mut(&FontFamily::Monospace) {
        vec.insert(0, "jetbrains_mono".to_owned());
        vec.insert(1, "dejavu_sans".to_owned());
    }
    ctx.set_fonts(fonts);
}

fn setup_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(10.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 7.0);
    style.spacing.window_margin = egui::Margin::same(14);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG_CANVAS;
    visuals.window_fill = BG_CARD;
    visuals.widgets.noninteractive.bg_fill = BG_CARD;
    visuals.widgets.inactive.bg_fill = BG_SURFACE;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER_HAIR);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(31, 37, 54);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(56, 66, 94));
    visuals.widgets.active.bg_fill = Color32::from_rgb(20, 74, 102);
    visuals.selection.bg_fill = Color32::from_rgb(20, 74, 102);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(7);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(7);
    visuals.widgets.active.corner_radius = CornerRadius::same(7);
    style.visuals = visuals;
    ctx.set_style(style);
}
