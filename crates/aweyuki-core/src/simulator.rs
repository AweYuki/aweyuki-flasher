use crate::boot_avb::{inspect_vbmeta, patch_vbmeta_verity_flags};
use crate::detector::{format_bytes_human, DetectedRomPackage};
use crate::error::{FlashError, Result};
use crate::planner::{
    resolve_step_source, DeviceMode, DeviceTargetInfo, FlashOptions, FlashPlan, FlashStep,
};
use crate::reader::SourceReader;
use crate::sparse::{auto_split_image, inspect_sparse, is_sparse_magic};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub trait FlashTransport {
    fn query_device(&mut self) -> Result<DeviceTargetInfo>;
    fn flash_blob(
        &mut self,
        partition: &str,
        data: &[u8],
        disable_verity: bool,
        disable_verification: bool,
        log: &mut dyn FnMut(&str),
    ) -> Result<()>;
    fn reboot_fastbootd(&mut self, log: &mut dyn FnMut(&str)) -> Result<()>;
    fn delete_logical(&mut self, name: &str, log: &mut dyn FnMut(&str)) -> Result<()>;
    fn create_logical(
        &mut self,
        name: &str,
        size_bytes: u64,
        log: &mut dyn FnMut(&str),
    ) -> Result<()>;
    fn resize_logical(
        &mut self,
        name: &str,
        size_bytes: u64,
        log: &mut dyn FnMut(&str),
    ) -> Result<()>;
    fn set_active_slot(&mut self, slot: &str, log: &mut dyn FnMut(&str)) -> Result<()>;
    fn reboot_sideload(&mut self, log: &mut dyn FnMut(&str)) -> Result<()>;
    fn adb_sideload(
        &mut self,
        package_name: &str,
        data: &[u8],
        log: &mut dyn FnMut(&str),
    ) -> Result<()>;
    fn wipe_userdata(&mut self, log: &mut dyn FnMut(&str)) -> Result<()>;
    fn reboot_system(&mut self, log: &mut dyn FnMut(&str)) -> Result<()>;
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum SimulatorProfile {
    Pixel9ProAB,
    Xiaomi14TProHyperOS,
    LowBufferOtg256K,
    RecoverySideloadDevice,
}

impl SimulatorProfile {
    pub fn all() -> &'static [Self] {
        &[
            Self::Pixel9ProAB,
            Self::Xiaomi14TProHyperOS,
            Self::LowBufferOtg256K,
            Self::RecoverySideloadDevice,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Pixel9ProAB => "Pixel 9 Pro (A/B • 256MB max-dl)",
            Self::Xiaomi14TProHyperOS => "Xiaomi 14T Pro (HyperOS • 512MB)",
            Self::LowBufferOtg256K => "Termux OTG Low-Buffer Demo (256 KB max-dl)",
            Self::RecoverySideloadDevice => "LineageOS Recovery (ADB Sideload)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeviceSimulator {
    pub profile: SimulatorProfile,
    pub info: DeviceTargetInfo,
    pub flashed_partitions: BTreeMap<String, u64>,
    pub logical_sizes: BTreeMap<String, u64>,
    pub last_sideloaded: Option<String>,
}

impl DeviceSimulator {
    pub fn from_profile(profile: SimulatorProfile) -> Self {
        let (info, logicals) = match profile {
            SimulatorProfile::Pixel9ProAB => (
                DeviceTargetInfo {
                    serial: "4821A0E-SIM-P9P".into(),
                    product: "caiman".into(),
                    mode: DeviceMode::Bootloader,
                    current_slot: Some("a".into()),
                    is_ab: true,
                    max_download_size: 256 * 1024 * 1024,
                    existing_logical_partitions: vec![
                        "system_a".into(),
                        "system_a-cow".into(),
                        "vendor_a".into(),
                        "product_a".into(),
                    ],
                },
                vec![
                    ("system_a", 512 * 1024),
                    ("system_a-cow", 64 * 1024),
                    ("vendor_a", 256 * 1024),
                    ("product_a", 192 * 1024),
                ],
            ),
            SimulatorProfile::Xiaomi14TProHyperOS => (
                DeviceTargetInfo {
                    serial: "XM14TP-SIM-9901".into(),
                    product: "rothko".into(),
                    mode: DeviceMode::Bootloader,
                    current_slot: Some("b".into()),
                    is_ab: true,
                    max_download_size: 512 * 1024 * 1024,
                    existing_logical_partitions: vec![
                        "system_b".into(),
                        "vendor_b".into(),
                        "product_b".into(),
                        "mi_ext_b".into(),
                    ],
                },
                vec![
                    ("system_b", 512 * 1024),
                    ("vendor_b", 256 * 1024),
                    ("product_b", 192 * 1024),
                    ("mi_ext_b", 64 * 1024),
                ],
            ),
            SimulatorProfile::LowBufferOtg256K => (
                DeviceTargetInfo {
                    serial: "TERMUX-OTG-256K".into(),
                    product: "aweyuki_demo".into(),
                    mode: DeviceMode::Bootloader,
                    current_slot: Some("a".into()),
                    is_ab: true,
                    max_download_size: 256 * 1024, // Forces visible multi-chunk sparse splitting on demo ROMs!
                    existing_logical_partitions: vec![
                        "system_a".into(),
                        "system_a-cow".into(),
                        "vendor_a".into(),
                    ],
                },
                vec![
                    ("system_a", 256 * 1024),
                    ("system_a-cow", 32 * 1024),
                    ("vendor_a", 128 * 1024),
                ],
            ),
            SimulatorProfile::RecoverySideloadDevice => (
                DeviceTargetInfo {
                    serial: "LOS-REC-SIDELOAD".into(),
                    product: "aweyuki_demo".into(),
                    mode: DeviceMode::RecoverySideload,
                    current_slot: Some("a".into()),
                    is_ab: true,
                    max_download_size: 256 * 1024 * 1024,
                    existing_logical_partitions: vec!["system_a".into(), "vendor_a".into()],
                },
                vec![("system_a", 512 * 1024), ("vendor_a", 256 * 1024)],
            ),
        };

        let logical_sizes = logicals
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();

        Self {
            profile,
            info,
            flashed_partitions: BTreeMap::new(),
            logical_sizes,
            last_sideloaded: None,
        }
    }
}

impl FlashTransport for DeviceSimulator {
    fn query_device(&mut self) -> Result<DeviceTargetInfo> {
        Ok(self.info.clone())
    }

    fn flash_blob(
        &mut self,
        partition: &str,
        data: &[u8],
        disable_verity: bool,
        disable_verification: bool,
        log: &mut dyn FnMut(&str),
    ) -> Result<()> {
        if data.len() as u64 > self.info.max_download_size {
            return Err(FlashError::SizeLimitExceeded(format!(
                "Device rejected transfer of {} bytes (> max-download-size {} bytes)",
                data.len(),
                self.info.max_download_size
            )));
        }

        let sparse_desc = if is_sparse_magic(data) {
            let mut r = SourceReader::from_bytes(data.to_vec());
            let sp = inspect_sparse(&mut r)?;
            format!(
                "sparse ({} chunks, raw {})",
                sp.header.total_chunks,
                format_bytes_human(sp.raw_size)
            )
        } else {
            format!("raw ({})", format_bytes_human(data.len() as u64))
        };

        let mut flags_note = String::new();
        if disable_verity || disable_verification {
            flags_note.push_str(" [AVB verity/verification disabled]");
        }

        log(&format!(
            "[SIM:{}] Sending '{}' ({}) -> Writing '{}'... OKAY{flags_note}",
            self.info.mode.label(),
            partition,
            sparse_desc,
            partition
        ));
        *self.flashed_partitions.entry(partition.to_string()).or_insert(0) += data.len() as u64;
        Ok(())
    }

    fn reboot_fastbootd(&mut self, log: &mut dyn FnMut(&str)) -> Result<()> {
        log("[SIM] fastboot reboot fastboot -> Rebooting into userspace fastbootd... OKAY");
        self.info.mode = DeviceMode::Fastbootd;
        Ok(())
    }

    fn delete_logical(&mut self, name: &str, log: &mut dyn FnMut(&str)) -> Result<()> {
        self.logical_sizes.remove(name);
        self.info.existing_logical_partitions.retain(|n| n != name);
        log(&format!(
            "[SIM:FASTBOOTD] delete-logical-partition '{name}'... OKAY"
        ));
        Ok(())
    }

    fn create_logical(
        &mut self,
        name: &str,
        size_bytes: u64,
        log: &mut dyn FnMut(&str),
    ) -> Result<()> {
        self.logical_sizes.insert(name.to_string(), size_bytes);
        if !self
            .info
            .existing_logical_partitions
            .contains(&name.to_string())
        {
            self.info.existing_logical_partitions.push(name.to_string());
        }
        log(&format!(
            "[SIM:FASTBOOTD] create-logical-partition '{name}' ({size_bytes} bytes)... OKAY"
        ));
        Ok(())
    }

    fn resize_logical(
        &mut self,
        name: &str,
        size_bytes: u64,
        log: &mut dyn FnMut(&str),
    ) -> Result<()> {
        self.logical_sizes.insert(name.to_string(), size_bytes);
        log(&format!(
            "[SIM:FASTBOOTD] resize-logical-partition '{name}' -> {}... OKAY",
            format_bytes_human(size_bytes)
        ));
        Ok(())
    }

    fn set_active_slot(&mut self, slot: &str, log: &mut dyn FnMut(&str)) -> Result<()> {
        self.info.current_slot = Some(slot.to_string());
        log(&format!("[SIM] Setting current slot to '{slot}'... OKAY"));
        Ok(())
    }

    fn reboot_sideload(&mut self, log: &mut dyn FnMut(&str)) -> Result<()> {
        log("[SIM] adb reboot sideload -> Waiting for recovery sideload... READY");
        self.info.mode = DeviceMode::RecoverySideload;
        Ok(())
    }

    fn adb_sideload(
        &mut self,
        package_name: &str,
        data: &[u8],
        log: &mut dyn FnMut(&str),
    ) -> Result<()> {
        let total = data.len();
        for pct in [15, 42, 74, 94, 100] {
            log(&format!(
                "[SIM:SIDELOAD] serving: '{package_name}' (~{pct}% of {})",
                format_bytes_human(total as u64)
            ));
        }
        log("[SIM:SIDELOAD] Total xfer: 1.00x -> Install completed with status 0.");
        self.last_sideloaded = Some(package_name.to_string());
        Ok(())
    }

    fn wipe_userdata(&mut self, log: &mut dyn FnMut(&str)) -> Result<()> {
        log("[SIM] Erasing 'userdata' & 'metadata' (format f2fs)... OKAY");
        Ok(())
    }

    fn reboot_system(&mut self, log: &mut dyn FnMut(&str)) -> Result<()> {
        log("[SIM] fastboot reboot -> Finished. Total simulated device boot complete.");
        self.info.mode = DeviceMode::Adb;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashExecutionReport {
    pub steps_completed: usize,
    pub total_bytes_transferred: u64,
    pub chunks_sent: usize,
    pub dry_run: bool,
}

pub fn execute_flash_plan(
    pkg: &DetectedRomPackage,
    plan: &FlashPlan,
    options: &FlashOptions,
    transport: &mut dyn FlashTransport,
    dry_run: bool,
    mut on_progress: impl FnMut(usize, usize, &str),
    mut on_log: impl FnMut(&str),
) -> Result<FlashExecutionReport> {
    let total_steps = plan.steps.len().max(1);
    let mut total_bytes_transferred = 0u64;
    let mut chunks_sent = 0usize;

    on_log(&format!(
        "=== Aweyuki Flasher: {} ({}) | max-download-size={} ===",
        pkg.package_name,
        if dry_run { "DRY-RUN" } else { "LIVE" },
        format_bytes_human(plan.effective_max_download_size)
    ));

    for (idx, step) in plan.steps.iter().enumerate() {
        let summary = step.summary();
        on_progress(idx, total_steps, &summary);

        match step {
            FlashStep::FlashPartition {
                source_id,
                partition_name,
                target_partition,
                disable_avb_flags,
                ..
            } => {
                let src = resolve_step_source(pkg, source_id).ok_or_else(|| {
                    FlashError::PartitionNotFound(format!("Missing source for {source_id}"))
                })?;

                on_log(&format!(
                    "[{}/{}] Preparing '{}' -> target '{}'...",
                    idx + 1,
                    total_steps,
                    partition_name,
                    target_partition
                ));
                let mut bytes = src.materialize_bytes()?;

                if *disable_avb_flags {
                    if let Some(before) = inspect_vbmeta(&bytes) {
                        let new_flags = patch_vbmeta_verity_flags(
                            &mut bytes,
                            options.disable_verity,
                            options.disable_verification,
                        )?;
                        on_log(&format!(
                            "  -> AVB0 vbmeta patched: flags 0x{:02x} -> 0x{:02x}",
                            before.flags, new_flags
                        ));
                    }
                }

                let max_dl = plan.effective_max_download_size;
                if options.auto_sparse_split && (bytes.len() as u64) > max_dl {
                    let blobs = auto_split_image(SourceReader::from_bytes(bytes), max_dl)?;
                    on_log(&format!(
                        "  -> Split '{}' into {} sparse chunk(s) <= {}",
                        target_partition,
                        blobs.len(),
                        format_bytes_human(max_dl)
                    ));
                    for chunk in blobs {
                        on_log(&format!(
                            "     Chunk {}/{}: {} bytes (blocks {}..{})",
                            chunk.meta.index + 1,
                            chunk.meta.total_parts,
                            chunk.data.len(),
                            chunk.meta.start_block,
                            chunk.meta.start_block + chunk.meta.active_blocks
                        ));
                        total_bytes_transferred += chunk.data.len() as u64;
                        chunks_sent += 1;
                        if !dry_run {
                            transport.flash_blob(
                                target_partition,
                                &chunk.data,
                                *disable_avb_flags && options.disable_verity,
                                *disable_avb_flags && options.disable_verification,
                                &mut on_log,
                            )?;
                        }
                    }
                } else {
                    total_bytes_transferred += bytes.len() as u64;
                    chunks_sent += 1;
                    if !dry_run {
                        transport.flash_blob(
                            target_partition,
                            &bytes,
                            *disable_avb_flags && options.disable_verity,
                            *disable_avb_flags && options.disable_verification,
                            &mut on_log,
                        )?;
                    } else {
                        on_log(&format!(
                            "  -> [DRY-RUN] Verified '{}' ({})",
                            target_partition,
                            format_bytes_human(bytes.len() as u64)
                        ));
                    }
                }
            }
            FlashStep::RebootFastbootd => {
                if dry_run {
                    on_log(&format!("[{}/{}] [DRY-RUN] reboot fastboot", idx + 1, total_steps));
                } else {
                    transport.reboot_fastbootd(&mut on_log)?;
                }
            }
            FlashStep::DeleteLogicalPartition(name) => {
                if dry_run {
                    on_log(&format!(
                        "[{}/{}] [DRY-RUN] delete-logical-partition {name}",
                        idx + 1,
                        total_steps
                    ));
                } else {
                    transport.delete_logical(name, &mut on_log)?;
                }
            }
            FlashStep::CreateLogicalPartition { name, size_bytes } => {
                if dry_run {
                    on_log(&format!(
                        "[{}/{}] [DRY-RUN] create-logical-partition {name} {size_bytes}",
                        idx + 1,
                        total_steps
                    ));
                } else {
                    transport.create_logical(name, *size_bytes, &mut on_log)?;
                }
            }
            FlashStep::ResizeLogicalPartition { name, size_bytes } => {
                if dry_run {
                    on_log(&format!(
                        "[{}/{}] [DRY-RUN] resize-logical-partition {name} {size_bytes}",
                        idx + 1,
                        total_steps
                    ));
                } else {
                    transport.resize_logical(name, *size_bytes, &mut on_log)?;
                }
            }
            FlashStep::SetActiveSlot(slot) => {
                if dry_run {
                    on_log(&format!(
                        "[{}/{}] [DRY-RUN] set_active {slot}",
                        idx + 1,
                        total_steps
                    ));
                } else {
                    transport.set_active_slot(slot, &mut on_log)?;
                }
            }
            FlashStep::RebootSideload => {
                if dry_run {
                    on_log(&format!(
                        "[{}/{}] [DRY-RUN] adb reboot sideload",
                        idx + 1,
                        total_steps
                    ));
                } else {
                    transport.reboot_sideload(&mut on_log)?;
                }
            }
            FlashStep::AdbSideload { package_name, .. } => {
                let archive = pkg.sideload_archive.clone().or_else(|| {
                    pkg.partitions
                        .first()
                        .and_then(|p| p.source.open_reader().ok())
                });
                let bytes = archive
                    .ok_or_else(|| FlashError::InvalidFormat("No OTA zip archive available for sideload".into()))?
                    .read_all()?;
                total_bytes_transferred += bytes.len() as u64;
                chunks_sent += 1;
                if dry_run {
                    on_log(&format!(
                        "[{}/{}] [DRY-RUN] adb sideload {package_name} ({})",
                        idx + 1,
                        total_steps,
                        format_bytes_human(bytes.len() as u64)
                    ));
                } else {
                    transport.adb_sideload(package_name, &bytes, &mut on_log)?;
                }
            }
            FlashStep::WipeUserdata => {
                if dry_run {
                    on_log(&format!("[{}/{}] [DRY-RUN] fastboot -w", idx + 1, total_steps));
                } else {
                    transport.wipe_userdata(&mut on_log)?;
                }
            }
            FlashStep::RebootSystem => {
                if dry_run {
                    on_log(&format!("[{}/{}] [DRY-RUN] fastboot reboot", idx + 1, total_steps));
                } else {
                    transport.reboot_system(&mut on_log)?;
                }
            }
        }
    }

    on_progress(total_steps, total_steps, "Completed");
    on_log("=== Finished cleanly ===");

    Ok(FlashExecutionReport {
        steps_completed: plan.steps.len(),
        total_bytes_transferred,
        chunks_sent,
        dry_run,
    })
}
