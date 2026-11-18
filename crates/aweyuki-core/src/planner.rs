use crate::detector::{DetectedPartition, DetectedRomPackage, PartitionDataSource, PartitionTargetMode, RomPackageKind};
use crate::super_lp::{LogicalLayoutAction, SuperLayoutManager};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DeviceMode {
    Bootloader,
    Fastbootd,
    Adb,
    RecoverySideload,
    Disconnected,
}

impl DeviceMode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Bootloader => "BOOTLOADER",
            Self::Fastbootd => "FASTBOOTD",
            Self::Adb => "ADB",
            Self::RecoverySideload => "SIDELOAD",
            Self::Disconnected => "OFFLINE",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SlotSelection {
    Active,
    SlotA,
    SlotB,
    BothSlots,
}

impl SlotSelection {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Active => "Active Slot",
            Self::SlotA => "Slot A (_a)",
            Self::SlotB => "Slot B (_b)",
            Self::BothSlots => "Both Slots (_a + _b)",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SuperFlashMode {
    WholeSparse,
    UnpackToFastbootd,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashOptions {
    pub slot: SlotSelection,
    pub max_download_size: u64,
    pub auto_sparse_split: bool,
    pub disable_verity: bool,
    pub disable_verification: bool,
    pub super_mode: SuperFlashMode,
    pub auto_resize_logical: bool,
    pub reclaim_cow: bool,
    pub wipe_userdata: bool,
    pub force_sideload: bool,
    pub reboot_after: bool,
}

impl Default for FlashOptions {
    fn default() -> Self {
        Self {
            slot: SlotSelection::Active,
            max_download_size: 256 * 1024 * 1024,
            auto_sparse_split: true,
            disable_verity: true,
            disable_verification: true,
            super_mode: SuperFlashMode::WholeSparse,
            auto_resize_logical: true,
            reclaim_cow: true,
            wipe_userdata: false,
            force_sideload: false,
            reboot_after: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceTargetInfo {
    pub serial: String,
    pub product: String,
    pub mode: DeviceMode,
    pub current_slot: Option<String>,
    pub is_ab: bool,
    pub max_download_size: u64,
    pub existing_logical_partitions: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FlashStage {
    Bootloader,
    Transition,
    Fastbootd,
    Sideload,
    Finalize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FlashStep {
    FlashPartition {
        source_id: String,
        partition_name: String,
        target_partition: String,
        stage: FlashStage,
        disable_avb_flags: bool,
        estimated_raw_bytes: u64,
        estimated_chunks: usize,
        will_split: bool,
    },
    RebootFastbootd,
    DeleteLogicalPartition(String),
    CreateLogicalPartition {
        name: String,
        size_bytes: u64,
    },
    ResizeLogicalPartition {
        name: String,
        size_bytes: u64,
    },
    SetActiveSlot(String),
    RebootSideload,
    AdbSideload {
        package_name: String,
        total_bytes: u64,
    },
    WipeUserdata,
    RebootSystem,
}

impl FlashStep {
    pub fn stage(&self) -> FlashStage {
        match self {
            Self::FlashPartition { stage, .. } => *stage,
            Self::RebootFastbootd | Self::RebootSideload => FlashStage::Transition,
            Self::DeleteLogicalPartition(_)
            | Self::CreateLogicalPartition { .. }
            | Self::ResizeLogicalPartition { .. } => FlashStage::Fastbootd,
            Self::AdbSideload { .. } => FlashStage::Sideload,
            Self::SetActiveSlot(_) | Self::WipeUserdata | Self::RebootSystem => {
                FlashStage::Finalize
            }
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::FlashPartition {
                target_partition,
                estimated_chunks,
                will_split,
                disable_avb_flags,
                ..
            } => {
                let mut extra = Vec::new();
                if *will_split {
                    extra.push(format!("{estimated_chunks} sparse chunks"));
                }
                if *disable_avb_flags {
                    extra.push("--disable-verity --disable-verification".to_string());
                }
                if extra.is_empty() {
                    format!("fastboot flash {target_partition}")
                } else {
                    format!("fastboot flash {target_partition} [{}]", extra.join(", "))
                }
            }
            Self::RebootFastbootd => "fastboot reboot fastboot".into(),
            Self::DeleteLogicalPartition(name) => {
                format!("fastboot delete-logical-partition {name}")
            }
            Self::CreateLogicalPartition { name, size_bytes } => {
                format!("fastboot create-logical-partition {name} {size_bytes}")
            }
            Self::ResizeLogicalPartition { name, size_bytes } => {
                format!("fastboot resize-logical-partition {name} {size_bytes}")
            }
            Self::SetActiveSlot(s) => format!("fastboot set_active {s}"),
            Self::RebootSideload => "adb reboot sideload".into(),
            Self::AdbSideload { package_name, .. } => format!("adb sideload {package_name}"),
            Self::WipeUserdata => "fastboot -w (erase userdata + metadata)".into(),
            Self::RebootSystem => "fastboot reboot".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashPlan {
    pub package_name: String,
    pub effective_max_download_size: u64,
    pub steps: Vec<FlashStep>,
    pub notes: Vec<String>,
}

fn resolve_slot_suffixes(device: &DeviceTargetInfo, slot_sel: SlotSelection) -> Vec<String> {
    if !device.is_ab {
        return vec![String::new()];
    }
    match slot_sel {
        SlotSelection::Active => {
            let s = device.current_slot.as_deref().unwrap_or("a");
            vec![format!("_{}", s.trim_start_matches('_'))]
        }
        SlotSelection::SlotA => vec!["_a".into()],
        SlotSelection::SlotB => vec!["_b".into()],
        SlotSelection::BothSlots => vec!["_a".into(), "_b".into()],
    }
}

fn estimate_chunk_count(raw_bytes: u64, max_dl: u64, auto_split: bool) -> (usize, bool) {
    if !auto_split || raw_bytes <= max_dl || max_dl == 0 {
        return (1, false);
    }
    let usable = max_dl.saturating_sub(128).max(4096);
    let n = raw_bytes.div_ceil(usable) as usize;
    (n.max(1), n > 1)
}

fn priority_rank(name: &str) -> u8 {
    let base = name
        .strip_suffix("_a")
        .or_else(|| name.strip_suffix("_b"))
        .unwrap_or(name);
    match base {
        b if b.starts_with("vbmeta") => 1,
        "abl" | "xbl" | "xbl_config" | "tz" | "hyp" | "preloader" | "lk" => 2,
        "dtbo" | "boot" | "init_boot" | "vendor_boot" | "recovery" => 3,
        "modem" | "dsp" | "bluetooth" => 4,
        "super" => 5,
        _ => 10,
    }
}

pub fn build_flash_plan(
    pkg: &DetectedRomPackage,
    device: &DeviceTargetInfo,
    options: &FlashOptions,
) -> FlashPlan {
    let max_dl = options.max_download_size.min(device.max_download_size).max(65_536);
    let mut notes = Vec::new();
    let mut steps = Vec::new();

    if options.force_sideload || pkg.kind == RomPackageKind::RecoverySideloadZip || pkg.is_incremental_ota {
        if pkg.sideload_archive.is_some() || pkg.kind == RomPackageKind::RecoverySideloadZip {
            if device.mode != DeviceMode::RecoverySideload {
                steps.push(FlashStep::RebootSideload);
            }
            steps.push(FlashStep::AdbSideload {
                package_name: pkg.package_name.clone(),
                total_bytes: pkg.total_input_bytes,
            });
            if pkg.is_incremental_ota {
                notes.push("Incremental OTA detected: automatically routed to ADB Sideload.".into());
            }
            if options.wipe_userdata {
                notes.push("Note: Format data from Recovery menu if switching ROM bases.".into());
            }
            return FlashPlan {
                package_name: pkg.package_name.clone(),
                effective_max_download_size: max_dl,
                steps,
                notes,
            };
        }
    }

    let suffixes = resolve_slot_suffixes(device, options.slot);
    let mut bootloader_parts: Vec<&DetectedPartition> = Vec::new();
    let mut fastbootd_parts: Vec<(String, String, u64, bool)> = Vec::new(); // (source_id, target_name, raw_size, is_ab)

    for p in pkg.partitions.iter().filter(|p| p.selected) {
        if p.is_super && options.super_mode == SuperFlashMode::UnpackToFastbootd {
            if let Some(si) = &pkg.super_info {
                notes.push(format!(
                    "Super strategy: unpacking {} logical partition(s) to fastbootd.",
                    si.partitions.len()
                ));
                for lp in &si.partitions {
                    fastbootd_parts.push((
                        format!("super_unpack:{}", lp.name),
                        lp.name.clone(),
                        lp.size_bytes,
                        false,
                    ));
                }
                continue;
            }
        }

        match p.target_mode {
            PartitionTargetMode::Bootloader => bootloader_parts.push(p),
            PartitionTargetMode::FastbootdLogical => {
                let already_slotted =
                    p.partition_name.ends_with("_a") || p.partition_name.ends_with("_b");
                fastbootd_parts.push((
                    p.id.clone(),
                    p.partition_name.clone(),
                    p.raw_size,
                    p.is_slot_ab && !already_slotted,
                ));
            }
            PartitionTargetMode::SideloadOnly => {}
        }
    }

    bootloader_parts.sort_by_key(|p| (priority_rank(&p.partition_name), p.partition_name.clone()));

    // Bootloader stage
    for p in bootloader_parts {
        let disable_avb = p.is_vbmeta && (options.disable_verity || options.disable_verification);
        let size_for_split = p.sparse_size.unwrap_or(p.raw_size);
        let (chunks, will_split) =
            estimate_chunk_count(size_for_split, max_dl, options.auto_sparse_split);

        if p.is_slot_ab && device.is_ab && !p.partition_name.ends_with("_a") && !p.partition_name.ends_with("_b") {
            for suf in &suffixes {
                steps.push(FlashStep::FlashPartition {
                    source_id: p.id.clone(),
                    partition_name: p.partition_name.clone(),
                    target_partition: format!("{}{}", p.partition_name, suf),
                    stage: FlashStage::Bootloader,
                    disable_avb_flags: disable_avb,
                    estimated_raw_bytes: p.raw_size,
                    estimated_chunks: chunks,
                    will_split,
                });
            }
        } else {
            steps.push(FlashStep::FlashPartition {
                source_id: p.id.clone(),
                partition_name: p.partition_name.clone(),
                target_partition: p.partition_name.clone(),
                stage: FlashStage::Bootloader,
                disable_avb_flags: disable_avb,
                estimated_raw_bytes: p.raw_size,
                estimated_chunks: chunks,
                will_split,
            });
        }
    }

    // Fastbootd logical partitions stage
    if !fastbootd_parts.is_empty() {
        if device.mode != DeviceMode::Fastbootd {
            steps.push(FlashStep::RebootFastbootd);
        }

        let mut target_logicals = Vec::new();
        let mut flash_targets = Vec::new();

        for (src_id, base_name, raw_size, needs_slot) in &fastbootd_parts {
            if *needs_slot && device.is_ab {
                for suf in &suffixes {
                    let target = format!("{base_name}{suf}");
                    target_logicals.push((target.clone(), *raw_size));
                    flash_targets.push((src_id.clone(), base_name.clone(), target, *raw_size));
                }
            } else {
                target_logicals.push((base_name.clone(), *raw_size));
                flash_targets.push((src_id.clone(), base_name.clone(), base_name.clone(), *raw_size));
            }
        }

        if options.auto_resize_logical {
            for act in SuperLayoutManager::plan_resize_actions(
                &device.existing_logical_partitions,
                &target_logicals,
                options.reclaim_cow,
            ) {
                match act {
                    LogicalLayoutAction::DeletePartition(n) => {
                        steps.push(FlashStep::DeleteLogicalPartition(n));
                    }
                    LogicalLayoutAction::CreatePartition { name, size_bytes } => {
                        steps.push(FlashStep::CreateLogicalPartition { name, size_bytes });
                    }
                    LogicalLayoutAction::ResizePartition { name, size_bytes } => {
                        steps.push(FlashStep::ResizeLogicalPartition { name, size_bytes });
                    }
                }
            }
        }

        for (src_id, part_name, target, raw_size) in flash_targets {
            let (chunks, will_split) =
                estimate_chunk_count(raw_size, max_dl, options.auto_sparse_split);
            steps.push(FlashStep::FlashPartition {
                source_id: src_id,
                partition_name: part_name,
                target_partition: target,
                stage: FlashStage::Fastbootd,
                disable_avb_flags: false,
                estimated_raw_bytes: raw_size,
                estimated_chunks: chunks,
                will_split,
            });
        }
    }

    if device.is_ab {
        match options.slot {
            SlotSelection::SlotA => steps.push(FlashStep::SetActiveSlot("a".into())),
            SlotSelection::SlotB => steps.push(FlashStep::SetActiveSlot("b".into())),
            _ => {}
        }
    }

    if options.wipe_userdata {
        steps.push(FlashStep::WipeUserdata);
    }

    if options.reboot_after && !steps.is_empty() {
        steps.push(FlashStep::RebootSystem);
    }

    FlashPlan {
        package_name: pkg.package_name.clone(),
        effective_max_download_size: max_dl,
        steps,
        notes,
    }
}

/// Resolve actual `PartitionDataSource` for a plan step (including unpacked `super.img` partitions).
pub fn resolve_step_source(
    pkg: &DetectedRomPackage,
    source_id: &str,
) -> Option<PartitionDataSource> {
    if let Some(lp_name) = source_id.strip_prefix("super_unpack:") {
        if let Some(super_part) = pkg.partitions.iter().find(|p| p.is_super) {
            if let Ok(super_reader) = super_part.source.open_reader() {
                return Some(PartitionDataSource::SuperLogicalPartition {
                    super_reader,
                    partition_name: lp_name.to_string(),
                });
            }
        }
    }
    pkg.partitions
        .iter()
        .find(|p| p.id == source_id)
        .map(|p| p.source.clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptFormat {
    Bash,
    WindowsBatch,
    TermuxOtg,
}

pub fn export_script(plan: &FlashPlan, format: ScriptFormat) -> String {
    let mut out = String::new();
    match format {
        ScriptFormat::Bash => {
            out.push_str("#!/usr/bin/env bash\nset -euo pipefail\n");
            out.push_str("# Generated by Aweyuki Flasher\n");
            out.push_str(&format!("# Package: {}\n", plan.package_name));
            out.push_str(&format!(
                "MAX_DL=\"{}\"\nFASTBOOT=\"${{FASTBOOT:-fastboot}}\"\nADB=\"${{ADB:-adb}}\"\nIMG_DIR=\"${{1:-.}}\"\n\n",
                plan.effective_max_download_size
            ));
        }
        ScriptFormat::WindowsBatch => {
            out.push_str("@echo off\r\nsetlocal enabledelayedexpansion\r\n");
            out.push_str(":: Generated by Aweyuki Flasher\r\n");
            out.push_str(&format!(":: Package: {}\r\n", plan.package_name));
            out.push_str("if \"%~1\"==\"\" (set \"IMG_DIR=.\") else (set \"IMG_DIR=%~1\")\r\n");
            out.push_str("set \"FASTBOOT=fastboot\"\r\nset \"ADB=adb\"\r\n\r\n");
        }
        ScriptFormat::TermuxOtg => {
            out.push_str("#!/data/data/com.termux/files/usr/bin/bash\nset -euo pipefail\n");
            out.push_str("# Generated by Aweyuki Flasher (Termux Android-to-Android OTG)\n");
            out.push_str(&format!("# Package: {}\n", plan.package_name));
            out.push_str("IMG_DIR=\"${1:-.}\"\n");
            out.push_str("if command -v termux-fastboot >/dev/null 2>&1; then FASTBOOT=termux-fastboot; else FASTBOOT=fastboot; fi\n");
            out.push_str("if command -v termux-adb >/dev/null 2>&1; then ADB=termux-adb; else ADB=adb; fi\n");
            out.push_str("if [ -z \"${TERMUX_USB_FD:-}\" ] && command -v termux-usb >/dev/null 2>&1; then\n");
            out.push_str("  USB_DEV=$(termux-usb -l | tr -d '[]\" ,' | head -n1)\n");
            out.push_str("  [ -n \"$USB_DEV\" ] && exec termux-usb -r -e \"$0 $IMG_DIR\" \"$USB_DEV\"\n");
            out.push_str("fi\n\n");
        }
    }

    let nl = if format == ScriptFormat::WindowsBatch {
        "\r\n"
    } else {
        "\n"
    };

    for step in &plan.steps {
        let cmd = match step {
            FlashStep::FlashPartition {
                partition_name,
                target_partition,
                disable_avb_flags,
                will_split,
                estimated_chunks,
                ..
            } => {
                let mut flags = String::new();
                if *disable_avb_flags {
                    flags.push_str("--disable-verity --disable-verification ");
                }
                if format == ScriptFormat::WindowsBatch {
                    if *will_split {
                        format!(
                            "echo [Aweyuki] Flashing {estimated_chunks} sparse chunks for {target_partition}...{nl}for /L %%i in (0,1,{}) do %FASTBOOT% {flags}flash {target_partition} \"%IMG_DIR%\\{partition_name}_chunk_%%i.simg\" || exit /b 1",
                            estimated_chunks.saturating_sub(1)
                        )
                    } else {
                        format!(
                            "%FASTBOOT% {flags}flash {target_partition} \"%IMG_DIR%\\{partition_name}.img\" || exit /b 1"
                        )
                    }
                } else if *will_split {
                    format!(
                        "echo \"[Aweyuki] Flashing {estimated_chunks} sparse chunks for {target_partition}...\"{nl}for i in $(seq 0 {}); do $FASTBOOT {flags}flash {target_partition} \"$IMG_DIR/{partition_name}_chunk_${{i}}.simg\"; done",
                        estimated_chunks.saturating_sub(1)
                    )
                } else {
                    format!("$FASTBOOT {flags}flash {target_partition} \"$IMG_DIR/{partition_name}.img\"")
                }
            }
            FlashStep::RebootFastbootd => {
                if format == ScriptFormat::WindowsBatch {
                    "%FASTBOOT% reboot fastboot || exit /b 1".into()
                } else {
                    "$FASTBOOT reboot fastboot".into()
                }
            }
            FlashStep::DeleteLogicalPartition(n) => {
                if format == ScriptFormat::WindowsBatch {
                    format!("%FASTBOOT% delete-logical-partition {n} || exit /b 0")
                } else {
                    format!("$FASTBOOT delete-logical-partition {n} || true")
                }
            }
            FlashStep::CreateLogicalPartition { name, size_bytes } => {
                if format == ScriptFormat::WindowsBatch {
                    format!("%FASTBOOT% create-logical-partition {name} {size_bytes} || exit /b 1")
                } else {
                    format!("$FASTBOOT create-logical-partition {name} {size_bytes}")
                }
            }
            FlashStep::ResizeLogicalPartition { name, size_bytes } => {
                if format == ScriptFormat::WindowsBatch {
                    format!("%FASTBOOT% resize-logical-partition {name} {size_bytes} || exit /b 1")
                } else {
                    format!("$FASTBOOT resize-logical-partition {name} {size_bytes}")
                }
            }
            FlashStep::SetActiveSlot(s) => {
                if format == ScriptFormat::WindowsBatch {
                    format!("%FASTBOOT% set_active {s} || exit /b 1")
                } else {
                    format!("$FASTBOOT set_active {s}")
                }
            }
            FlashStep::RebootSideload => {
                if format == ScriptFormat::WindowsBatch {
                    "%ADB% reboot sideload || exit /b 1".into()
                } else {
                    "$ADB reboot sideload".into()
                }
            }
            FlashStep::AdbSideload { package_name, .. } => {
                if format == ScriptFormat::WindowsBatch {
                    format!("%ADB% sideload \"{package_name}\" || exit /b 1")
                } else {
                    format!("$ADB sideload \"{package_name}\"")
                }
            }
            FlashStep::WipeUserdata => {
                if format == ScriptFormat::WindowsBatch {
                    "%FASTBOOT% -w || exit /b 1".into()
                } else {
                    "$FASTBOOT -w".into()
                }
            }
            FlashStep::RebootSystem => {
                if format == ScriptFormat::WindowsBatch {
                    "%FASTBOOT% reboot".into()
                } else {
                    "$FASTBOOT reboot".into()
                }
            }
        };
        out.push_str(&cmd);
        out.push_str(nl);
    }
    out
}
