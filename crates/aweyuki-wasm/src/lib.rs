use aweyuki_core::boot_avb::patch_vbmeta_verity_flags;
use aweyuki_core::demo_samples::{generate_sample_rom, SampleKind};
use aweyuki_core::detector::{
    detect_from_bytes, format_bytes_human, DetectedFormat, DetectedRomPackage, PartitionTargetMode,
};
use aweyuki_core::planner::{
    build_flash_plan, export_script, FlashOptions, FlashPlan, FlashStage, FlashStep, ScriptFormat,
    SlotSelection, SuperFlashMode,
};
use aweyuki_core::reader::SourceReader;
use aweyuki_core::simulator::{execute_flash_plan, DeviceSimulator, SimulatorProfile};
use aweyuki_core::sparse::{auto_split_image, VisualSegmentKind};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

#[derive(Serialize)]
struct DeviceStateJson {
    serial: String,
    product: String,
    mode: String,
    current_slot: Option<String>,
    is_ab: bool,
    max_download_size: u64,
    max_download_human: String,
    profile_id: String,
    logical_partitions: Vec<String>,
}

#[derive(Serialize)]
struct OptionsJson {
    slot: String,
    max_download_size: u64,
    auto_sparse_split: bool,
    disable_verity: bool,
    super_mode: String,
    auto_resize_logical: bool,
    force_sideload: bool,
    wipe_userdata: bool,
    reboot_after: bool,
}

#[derive(Deserialize)]
struct OptionsPatchJson {
    slot: Option<String>,
    max_download_size: Option<u64>,
    auto_sparse_split: Option<bool>,
    disable_verity: Option<bool>,
    super_mode: Option<String>,
    auto_resize_logical: Option<bool>,
    force_sideload: Option<bool>,
    wipe_userdata: Option<bool>,
    reboot_after: Option<bool>,
}

#[derive(Serialize)]
struct PartitionJson {
    index: usize,
    name: String,
    format: String,
    size_bytes: u64,
    size_human: String,
    stage: String,
    selected: bool,
    is_vbmeta: bool,
    is_super: bool,
    can_unpack: bool,
    est_chunks: u64,
    notes: String,
    sha256: Option<String>,
}

#[derive(Serialize)]
struct SuperGroupJson {
    name: String,
    used_bytes: u64,
    used_human: String,
    maximum_size: u64,
    maximum_human: String,
}

#[derive(Serialize)]
struct SuperPartJson {
    name: String,
    group_name: String,
    size_bytes: u64,
    size_human: String,
    extents_count: usize,
}

#[derive(Serialize)]
struct SuperInfoJson {
    is_sparse: bool,
    raw_size: u64,
    raw_human: String,
    groups: Vec<SuperGroupJson>,
    partitions: Vec<SuperPartJson>,
}

#[derive(Serialize)]
struct PackageJson {
    package_name: String,
    kind_label: String,
    total_bytes: u64,
    total_human: String,
    codename_hint: Option<String>,
    is_incremental: bool,
    stripped_locks: Vec<String>,
    warnings: Vec<String>,
    partitions: Vec<PartitionJson>,
    super_info: Option<SuperInfoJson>,
}

#[derive(Serialize)]
struct StageCountsJson {
    bootloader: usize,
    transition: usize,
    fastbootd: usize,
    sideload: usize,
    finalize: usize,
}

#[derive(Serialize)]
struct PlanStepJson {
    num: usize,
    stage: String,
    summary: String,
}

#[derive(Serialize)]
struct PlanJson {
    effective_max_dl: u64,
    effective_max_dl_human: String,
    stage_counts: StageCountsJson,
    steps: Vec<PlanStepJson>,
    notes: Vec<String>,
}

#[derive(Serialize)]
struct StudioStateJson {
    device: DeviceStateJson,
    options: OptionsJson,
    package: Option<PackageJson>,
    plan: Option<PlanJson>,
}

#[derive(Serialize)]
struct SplitSegmentJson {
    kind: String,
    start_block: u32,
    block_count: u32,
}

#[derive(Serialize)]
struct SplitChunkJson {
    index: usize,
    total_parts: usize,
    start_block: u32,
    active_blocks: u32,
    total_blocks: u32,
    encoded_bytes: usize,
    encoded_human: String,
    segments: Vec<SplitSegmentJson>,
}

#[derive(Serialize)]
struct SplitPreviewJson {
    partition_name: String,
    raw_size: u64,
    raw_human: String,
    max_dl: u64,
    max_dl_human: String,
    total_encoded_bytes: usize,
    total_encoded_human: String,
    error: Option<String>,
    chunks: Vec<SplitChunkJson>,
}

#[derive(Serialize)]
struct FlashReportJson {
    steps_completed: usize,
    total_bytes_transferred: u64,
    total_human: String,
    chunks_sent: usize,
    dry_run: bool,
}

#[derive(Serialize)]
struct FlashRunJson {
    report: FlashReportJson,
    logs: Vec<String>,
    state_after: StudioStateJson,
}

fn profile_to_id(p: SimulatorProfile) -> &'static str {
    match p {
        SimulatorProfile::Pixel9ProAB => "pixel9",
        SimulatorProfile::Xiaomi14TProHyperOS => "xiaomi",
        SimulatorProfile::LowBufferOtg256K => "low-buffer",
        SimulatorProfile::RecoverySideloadDevice => "sideload",
    }
}

fn id_to_profile(s: &str) -> SimulatorProfile {
    match s {
        "pixel9" => SimulatorProfile::Pixel9ProAB,
        "xiaomi" => SimulatorProfile::Xiaomi14TProHyperOS,
        "sideload" => SimulatorProfile::RecoverySideloadDevice,
        _ => SimulatorProfile::LowBufferOtg256K,
    }
}

#[wasm_bindgen]
pub struct WasmStudioSession {
    profile: SimulatorProfile,
    simulator: DeviceSimulator,
    options: FlashOptions,
    package: Option<DetectedRomPackage>,
    plan: Option<FlashPlan>,
}

impl Default for WasmStudioSession {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmStudioSession {
    fn recompute_plan(&mut self) {
        if let Some(pkg) = &self.package {
            self.plan = Some(build_flash_plan(pkg, &self.simulator.info, &self.options));
        } else {
            self.plan = None;
        }
    }

    fn build_state(&self) -> StudioStateJson {
        let dev = &self.simulator.info;
        let max_dl = self
            .options
            .max_download_size
            .min(dev.max_download_size)
            .max(65_536);

        let device = DeviceStateJson {
            serial: dev.serial.clone(),
            product: dev.product.clone(),
            mode: dev.mode.label().to_string(),
            current_slot: dev.current_slot.clone(),
            is_ab: dev.is_ab,
            max_download_size: dev.max_download_size,
            max_download_human: format_bytes_human(dev.max_download_size),
            profile_id: profile_to_id(self.profile).to_string(),
            logical_partitions: dev.existing_logical_partitions.clone(),
        };

        let options = OptionsJson {
            slot: match self.options.slot {
                SlotSelection::Active => "active",
                SlotSelection::SlotA => "a",
                SlotSelection::SlotB => "b",
                SlotSelection::BothSlots => "both",
            }
            .to_string(),
            max_download_size: self.options.max_download_size,
            auto_sparse_split: self.options.auto_sparse_split,
            disable_verity: self.options.disable_verity,
            super_mode: match self.options.super_mode {
                SuperFlashMode::WholeSparse => "whole",
                SuperFlashMode::UnpackToFastbootd => "unpack",
            }
            .to_string(),
            auto_resize_logical: self.options.auto_resize_logical,
            force_sideload: self.options.force_sideload,
            wipe_userdata: self.options.wipe_userdata,
            reboot_after: self.options.reboot_after,
        };

        let package = self.package.as_ref().map(|pkg| {
            let partitions = pkg
                .partitions
                .iter()
                .enumerate()
                .map(|(index, p)| {
                    let sz_eff = p.sparse_size.unwrap_or(p.raw_size);
                    let est_chunks = if self.options.auto_sparse_split && sz_eff > max_dl {
                        sz_eff.div_ceil(max_dl.saturating_sub(128).max(4096))
                    } else {
                        1
                    };
                    let stage = match p.target_mode {
                        PartitionTargetMode::Bootloader => "bootloader",
                        PartitionTargetMode::FastbootdLogical => "fastbootd",
                        PartitionTargetMode::SideloadOnly => "sideload",
                    };
                    PartitionJson {
                        index,
                        name: p.partition_name.clone(),
                        format: p.format.label().to_string(),
                        size_bytes: p.raw_size,
                        size_human: format_bytes_human(p.raw_size),
                        stage: stage.to_string(),
                        selected: p.selected,
                        is_vbmeta: p.is_vbmeta,
                        is_super: p.is_super,
                        can_unpack: p.format != DetectedFormat::SideloadPackage,
                        est_chunks,
                        notes: p.notes.clone(),
                        sha256: p.sha256_hex.clone(),
                    }
                })
                .collect();

            let super_info = pkg.super_info.as_ref().map(|si| SuperInfoJson {
                is_sparse: si.is_sparse,
                raw_size: si.raw_size,
                raw_human: format_bytes_human(si.raw_size),
                groups: si
                    .groups
                    .iter()
                    .filter(|g| g.name != "default")
                    .map(|g| SuperGroupJson {
                        name: g.name.clone(),
                        used_bytes: g.used_bytes,
                        used_human: format_bytes_human(g.used_bytes),
                        maximum_size: g.maximum_size,
                        maximum_human: format_bytes_human(g.maximum_size),
                    })
                    .collect(),
                partitions: si
                    .partitions
                    .iter()
                    .map(|lp| SuperPartJson {
                        name: lp.name.clone(),
                        group_name: lp.group_name.clone(),
                        size_bytes: lp.size_bytes,
                        size_human: format_bytes_human(lp.size_bytes),
                        extents_count: lp.extents.len(),
                    })
                    .collect(),
            });

            PackageJson {
                package_name: pkg.package_name.clone(),
                kind_label: pkg.kind.label().to_string(),
                total_bytes: pkg.total_input_bytes,
                total_human: format_bytes_human(pkg.total_input_bytes),
                codename_hint: pkg.codename_hint.clone(),
                is_incremental: pkg.is_incremental_ota,
                stripped_locks: pkg.stripped_lock_commands.clone(),
                warnings: pkg.warnings.clone(),
                partitions,
                super_info,
            }
        });

        let plan = self.plan.as_ref().map(|pl| {
            let mut bootloader = 0;
            let mut transition = 0;
            let mut fastbootd = 0;
            let mut sideload = 0;
            let mut finalize = 0;
            let steps = pl
                .steps
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let stage_str = match s.stage() {
                        FlashStage::Bootloader => {
                            bootloader += 1;
                            "bootloader"
                        }
                        FlashStage::Transition => {
                            transition += 1;
                            "transition"
                        }
                        FlashStage::Fastbootd => {
                            fastbootd += 1;
                            "fastbootd"
                        }
                        FlashStage::Sideload => {
                            sideload += 1;
                            "sideload"
                        }
                        FlashStage::Finalize => {
                            finalize += 1;
                            "finalize"
                        }
                    };
                    let _ = matches!(s, FlashStep::RebootSystem);
                    PlanStepJson {
                        num: i + 1,
                        stage: stage_str.to_string(),
                        summary: s.summary(),
                    }
                })
                .collect();

            PlanJson {
                effective_max_dl: pl.effective_max_download_size,
                effective_max_dl_human: format_bytes_human(pl.effective_max_download_size),
                stage_counts: StageCountsJson {
                    bootloader,
                    transition,
                    fastbootd,
                    sideload,
                    finalize,
                },
                steps,
                notes: pl.notes.clone(),
            }
        });

        StudioStateJson {
            device,
            options,
            package,
            plan,
        }
    }
}

#[wasm_bindgen]
impl WasmStudioSession {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        let profile = SimulatorProfile::LowBufferOtg256K;
        let simulator = DeviceSimulator::from_profile(profile);
        let options = FlashOptions {
            max_download_size: 256 * 1024,
            ..FlashOptions::default()
        };
        let mut s = Self {
            profile,
            simulator,
            options,
            package: None,
            plan: None,
        };
        let _ = s.load_sample("payload-zip");
        s
    }

    pub fn get_state(&self) -> String {
        serde_json::to_string(&self.build_state()).unwrap_or_else(|_| "{}".into())
    }

    pub fn load_sample(&mut self, kind: &str) -> Result<String, JsValue> {
        let sk = match kind {
            "payload" => SampleKind::StandalonePayload,
            "super" => SampleKind::SuperSparse,
            "fastboot-zip" => SampleKind::FastbootZip,
            "sideload-zip" => SampleKind::RecoverySideloadZip,
            _ => SampleKind::PayloadOtaZip,
        };
        if sk == SampleKind::RecoverySideloadZip {
            self.profile = SimulatorProfile::RecoverySideloadDevice;
            self.simulator = DeviceSimulator::from_profile(self.profile);
        } else if self.profile == SimulatorProfile::RecoverySideloadDevice {
            self.profile = SimulatorProfile::LowBufferOtg256K;
            self.simulator = DeviceSimulator::from_profile(self.profile);
        }
        let (name, bytes) =
            generate_sample_rom(sk).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let pkg =
            detect_from_bytes(name, bytes).map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.package = Some(pkg);
        self.recompute_plan();
        Ok(self.get_state())
    }

    pub fn load_rom_bytes(&mut self, name: &str, bytes: &[u8]) -> Result<String, JsValue> {
        let pkg = detect_from_bytes(name.to_string(), bytes.to_vec())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.package = Some(pkg);
        self.recompute_plan();
        Ok(self.get_state())
    }

    pub fn set_sim_profile(&mut self, profile: &str) -> String {
        let p = id_to_profile(profile);
        self.profile = p;
        self.simulator = DeviceSimulator::from_profile(p);
        self.options.max_download_size = if p == SimulatorProfile::LowBufferOtg256K {
            256 * 1024
        } else {
            self.simulator.info.max_download_size
        };
        self.recompute_plan();
        self.get_state()
    }

    pub fn set_options_json(&mut self, json: &str) -> Result<String, JsValue> {
        let patch: OptionsPatchJson =
            serde_json::from_str(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        if let Some(slot) = patch.slot {
            self.options.slot = match slot.as_str() {
                "a" => SlotSelection::SlotA,
                "b" => SlotSelection::SlotB,
                "both" => SlotSelection::BothSlots,
                _ => SlotSelection::Active,
            };
        }
        if let Some(m) = patch.max_download_size {
            self.options.max_download_size = m.max(65_536);
        }
        if let Some(v) = patch.auto_sparse_split {
            self.options.auto_sparse_split = v;
        }
        if let Some(v) = patch.disable_verity {
            self.options.disable_verity = v;
            self.options.disable_verification = v;
        }
        if let Some(sm) = patch.super_mode {
            self.options.super_mode = if sm == "unpack" {
                SuperFlashMode::UnpackToFastbootd
            } else {
                SuperFlashMode::WholeSparse
            };
        }
        if let Some(v) = patch.auto_resize_logical {
            self.options.auto_resize_logical = v;
            self.options.reclaim_cow = v;
        }
        if let Some(v) = patch.force_sideload {
            self.options.force_sideload = v;
        }
        if let Some(v) = patch.wipe_userdata {
            self.options.wipe_userdata = v;
        }
        if let Some(v) = patch.reboot_after {
            self.options.reboot_after = v;
        }
        self.recompute_plan();
        Ok(self.get_state())
    }

    pub fn set_partition_selected(&mut self, idx: usize, selected: bool) -> String {
        if let Some(pkg) = &mut self.package {
            if let Some(p) = pkg.partitions.get_mut(idx) {
                p.selected = selected;
            }
        }
        self.recompute_plan();
        self.get_state()
    }

    pub fn get_split_preview(&self, idx: usize) -> Result<String, JsValue> {
        let pkg = self
            .package
            .as_ref()
            .ok_or_else(|| JsValue::from_str("No ROM loaded"))?;
        let part = pkg
            .partitions
            .get(idx)
            .ok_or_else(|| JsValue::from_str("Partition index out of range"))?;
        let max_dl = self
            .options
            .max_download_size
            .min(self.simulator.info.max_download_size)
            .max(65_536);

        if part.raw_size > 32 * 1024 * 1024 {
            let preview = SplitPreviewJson {
                partition_name: part.partition_name.clone(),
                raw_size: part.raw_size,
                raw_human: format_bytes_human(part.raw_size),
                max_dl,
                max_dl_human: format_bytes_human(max_dl),
                total_encoded_bytes: 0,
                total_encoded_human: "0 B".into(),
                error: Some(format!(
                    "Partition '{}' ({}) exceeds 32 MiB browser preview cap; streaming split runs automatically during CLI/Desktop flash.",
                    part.partition_name,
                    format_bytes_human(part.raw_size)
                )),
                chunks: vec![],
            };
            return serde_json::to_string(&preview)
                .map_err(|e| JsValue::from_str(&e.to_string()));
        }

        let bytes = part
            .source
            .materialize_bytes()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let blobs = auto_split_image(SourceReader::from_bytes(bytes), max_dl)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        let total_encoded_bytes: usize = blobs.iter().map(|b| b.meta.encoded_bytes).sum();
        let chunks = blobs
            .into_iter()
            .map(|b| SplitChunkJson {
                index: b.meta.index,
                total_parts: b.meta.total_parts,
                start_block: b.meta.start_block,
                active_blocks: b.meta.active_blocks,
                total_blocks: b.meta.total_blocks,
                encoded_bytes: b.meta.encoded_bytes,
                encoded_human: format_bytes_human(b.meta.encoded_bytes as u64),
                segments: b
                    .meta
                    .segments
                    .into_iter()
                    .map(|seg| SplitSegmentJson {
                        kind: match seg.kind {
                            VisualSegmentKind::DontCareLead => "dont_care_lead",
                            VisualSegmentKind::Raw => "raw",
                            VisualSegmentKind::FillZero => "fill_zero",
                            VisualSegmentKind::DontCareMiddle => "dont_care_mid",
                            VisualSegmentKind::DontCareTail => "dont_care_tail",
                        }
                        .to_string(),
                        start_block: seg.start_block,
                        block_count: seg.block_count,
                    })
                    .collect(),
            })
            .collect();

        let preview = SplitPreviewJson {
            partition_name: part.partition_name.clone(),
            raw_size: part.raw_size,
            raw_human: format_bytes_human(part.raw_size),
            max_dl,
            max_dl_human: format_bytes_human(max_dl),
            total_encoded_bytes,
            total_encoded_human: format_bytes_human(total_encoded_bytes as u64),
            error: None,
            chunks,
        };
        serde_json::to_string(&preview).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    pub fn get_script(&self, fmt: &str) -> String {
        let Some(plan) = &self.plan else {
            return String::new();
        };
        let sf = match fmt {
            "bat" => ScriptFormat::WindowsBatch,
            "termux" => ScriptFormat::TermuxOtg,
            _ => ScriptFormat::Bash,
        };
        export_script(plan, sf)
    }

    pub fn extract_partition(&self, idx: usize) -> Result<Vec<u8>, JsValue> {
        let pkg = self
            .package
            .as_ref()
            .ok_or_else(|| JsValue::from_str("No ROM loaded"))?;
        let part = pkg
            .partitions
            .get(idx)
            .ok_or_else(|| JsValue::from_str("Partition index out of range"))?;
        let mut bytes = part
            .source
            .materialize_bytes()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        if part.is_vbmeta && (self.options.disable_verity || self.options.disable_verification) {
            let _ = patch_vbmeta_verity_flags(
                &mut bytes,
                self.options.disable_verity,
                self.options.disable_verification,
            );
        }
        Ok(bytes)
    }

    pub fn run_flash(&mut self, dry_run: bool) -> Result<String, JsValue> {
        let pkg = self
            .package
            .as_ref()
            .ok_or_else(|| JsValue::from_str("No ROM loaded"))?
            .clone();
        let plan = self
            .plan
            .as_ref()
            .ok_or_else(|| JsValue::from_str("No flash plan"))?
            .clone();
        let mut logs = Vec::new();
        let report = execute_flash_plan(
            &pkg,
            &plan,
            &self.options,
            &mut self.simulator,
            dry_run,
            |_, _, _| {},
            |line| logs.push(line.to_string()),
        )
        .map_err(|e| JsValue::from_str(&e.to_string()))?;

        let out = FlashRunJson {
            report: FlashReportJson {
                steps_completed: report.steps_completed,
                total_bytes_transferred: report.total_bytes_transferred,
                total_human: format_bytes_human(report.total_bytes_transferred),
                chunks_sent: report.chunks_sent,
                dry_run: report.dry_run,
            },
            logs,
            state_after: self.build_state(),
        };
        serde_json::to_string(&out).map_err(|e| JsValue::from_str(&e.to_string()))
    }
}
