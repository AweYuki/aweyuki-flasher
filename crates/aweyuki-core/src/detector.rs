use crate::boot_avb::{inspect_boot_header, inspect_vbmeta, BootImageKind};
use crate::error::{FlashError, Result};
#[cfg(not(target_arch = "wasm32"))]
use crate::reader::FileSource;
use crate::reader::SourceReader;
use crate::payload::{
    extract_payload_partition_bytes, is_payload_magic, parse_payload, ParsedPayload,
};
use crate::sideload::{inspect_sideload_zip, SideloadMetadata};
use crate::sparse::{inspect_sparse, is_sparse_magic, ImageRandomReader};
use crate::super_lp::{has_super_magic, inspect_super, inspect_super_reader, unpack_super_partition_bytes, SuperInfo};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::sync::Arc;
use zip::{CompressionMethod, ZipArchive};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RomPackageKind {
    PayloadOtaZip,
    StandalonePayloadBin,
    FastbootZip,
    FastbootDirectory,
    RecoverySideloadZip,
    StandaloneSuperImage,
    SinglePartitionImage,
}

impl RomPackageKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::PayloadOtaZip => "Android OTA ZIP (payload.bin)",
            Self::StandalonePayloadBin => "Standalone payload.bin (CrAU)",
            Self::FastbootZip => "Fastboot ROM ZIP",
            Self::FastbootDirectory => "Fastboot Image Directory",
            Self::RecoverySideloadZip => "Recovery Sideload OTA ZIP",
            Self::StandaloneSuperImage => "Dynamic Super Image (super.img)",
            Self::SinglePartitionImage => "Single Partition Image (.img)",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PartitionTargetMode {
    Bootloader,
    FastbootdLogical,
    SideloadOnly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DetectedFormat {
    Raw,
    Sparse,
    PayloadEncoded,
    SuperLogical,
    AndroidBoot,
    VendorBoot,
    AvbVbmeta,
    SideloadPackage,
}

impl DetectedFormat {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Raw => "RAW",
            Self::Sparse => "SPARSE",
            Self::PayloadEncoded => "PAYLOAD",
            Self::SuperLogical => "LP-LOGICAL",
            Self::AndroidBoot => "ANDROID!",
            Self::VendorBoot => "VNDRBOOT",
            Self::AvbVbmeta => "AVB0",
            Self::SideloadPackage => "OTA-ZIP",
        }
    }
}

#[derive(Clone)]
pub enum PartitionDataSource {
    Direct(SourceReader),
    ZipEntry {
        archive: SourceReader,
        entry_name: String,
    },
    PayloadPartition {
        payload_reader: SourceReader,
        parsed: Arc<ParsedPayload>,
        partition_name: String,
    },
    SuperLogicalPartition {
        super_reader: SourceReader,
        partition_name: String,
    },
}

impl PartitionDataSource {
    pub fn materialize_bytes(&self) -> Result<Vec<u8>> {
        match self {
            Self::Direct(r) => Ok(r.read_all()?),
            Self::ZipEntry {
                archive,
                entry_name,
            } => {
                let mut z = ZipArchive::new(archive.clone())?;
                let mut f = z.by_name(entry_name)?;
                let mut out = Vec::with_capacity(f.size() as usize);
                f.read_to_end(&mut out)?;
                Ok(out)
            }
            Self::PayloadPartition {
                payload_reader,
                parsed,
                partition_name,
            } => extract_payload_partition_bytes(payload_reader, parsed, partition_name),
            Self::SuperLogicalPartition {
                super_reader,
                partition_name,
            } => unpack_super_partition_bytes(super_reader.clone(), partition_name),
        }
    }

    pub fn open_reader(&self) -> Result<SourceReader> {
        match self {
            Self::Direct(r) => Ok(r.clone()),
            Self::ZipEntry {
                archive,
                entry_name,
            } => {
                let mut z = ZipArchive::new(archive.clone())?;
                let (compression, data_start, size) = {
                    let f = z.by_name(entry_name)?;
                    (f.compression(), f.data_start(), f.size())
                };
                if compression == CompressionMethod::Stored {
                    Ok(archive.slice(data_start, size)?)
                } else {
                    let mut f = z.by_name(entry_name)?;
                    let mut out = Vec::with_capacity(size as usize);
                    f.read_to_end(&mut out)?;
                    Ok(SourceReader::from_bytes(out))
                }
            }
            _ => Ok(SourceReader::from_bytes(self.materialize_bytes()?)),
        }
    }
}

#[derive(Clone)]
pub struct DetectedPartition {
    pub id: String,
    pub partition_name: String,
    pub source_label: String,
    pub raw_size: u64,
    pub sparse_size: Option<u64>,
    pub format: DetectedFormat,
    pub target_mode: PartitionTargetMode,
    pub is_slot_ab: bool,
    pub is_vbmeta: bool,
    pub is_super: bool,
    pub selected: bool,
    pub sha256_hex: Option<String>,
    pub notes: String,
    pub source: PartitionDataSource,
}

#[derive(Clone)]
pub struct DetectedRomPackage {
    pub package_name: String,
    pub kind: RomPackageKind,
    pub total_input_bytes: u64,
    pub codename_hint: Option<String>,
    pub is_incremental_ota: bool,
    pub partitions: Vec<DetectedPartition>,
    pub super_info: Option<SuperInfo>,
    pub payload_info: Option<Arc<ParsedPayload>>,
    pub sideload_meta: Option<SideloadMetadata>,
    pub sideload_archive: Option<SourceReader>,
    pub stripped_lock_commands: Vec<String>,
    pub warnings: Vec<String>,
    pub script_flash_order: Vec<(String, String)>,
}

const KNOWN_LOGICAL_BASES: &[&str] = &[
    "system",
    "system_ext",
    "vendor",
    "product",
    "odm",
    "system_dlkm",
    "vendor_dlkm",
    "odm_dlkm",
    "mi_ext",
    "my_product",
    "my_stock",
    "my_heytap",
    "my_carrier",
    "my_region",
    "my_engineering",
    "my_preload",
    "my_bigball",
    "my_manifest",
];

const KNOWN_AB_BOOTLOADER_BASES: &[&str] = &[
    "boot",
    "init_boot",
    "vendor_boot",
    "dtbo",
    "vbmeta",
    "vbmeta_system",
    "vbmeta_vendor",
    "pvmfw",
    "modem",
    "dsp",
    "bluetooth",
    "abl",
    "xbl",
    "xbl_config",
    "tz",
    "hyp",
    "keymaster",
    "devcfg",
    "qupfw",
    "uefisecapp",
    "featenabler",
    "imagefv",
    "shrm",
    "cpucp",
    "aop",
    "aop_config",
    "lk",
    "tee",
    "scp",
    "sspm",
    "spmfw",
    "mcupm",
    "dpm",
    "gz",
    "md1img",
    "pi_img",
    "cam_vpu1",
    "cam_vpu2",
    "cam_vpu3",
    "recovery",
];

pub fn normalize_partition_base(file_stem: &str) -> String {
    let lower = file_stem.to_ascii_lowercase();
    let stripped = lower
        .strip_suffix(".img")
        .or_else(|| lower.strip_suffix(".bin"))
        .unwrap_or(&lower);
    let stripped = stripped
        .split("_sparsechunk")
        .next()
        .unwrap_or(stripped);
    if stripped == "vbmeta_a" || stripped == "vbmeta_b" {
        return "vbmeta".to_string();
    }
    if stripped.ends_with("_a") || stripped.ends_with("_b") {
        // Keep super logical names if user explicitly passes system_a, else strip for base name
        let base = &stripped[..stripped.len() - 2];
        if KNOWN_LOGICAL_BASES.contains(&base) || KNOWN_AB_BOOTLOADER_BASES.contains(&base) {
            return base.to_string();
        }
    }
    stripped.to_string()
}

pub fn classify_partition_name(base: &str, dynamic_set: &BTreeSet<String>) -> (PartitionTargetMode, bool) {
    let clean = base
        .strip_suffix("_a")
        .or_else(|| base.strip_suffix("_b"))
        .unwrap_or(base);
    if dynamic_set.contains(clean) || dynamic_set.contains(base) || KNOWN_LOGICAL_BASES.contains(&clean) {
        return (PartitionTargetMode::FastbootdLogical, true);
    }
    if clean == "super" || clean == "userdata" || clean == "metadata" || clean == "cache" {
        return (PartitionTargetMode::Bootloader, false);
    }
    let ab = KNOWN_AB_BOOTLOADER_BASES.contains(&clean) || base.starts_with("vbmeta");
    (PartitionTargetMode::Bootloader, ab)
}

fn probe_image_reader(
    file_name: &str,
    explicit_part_name: Option<String>,
    reader: SourceReader,
    dynamic_set: &BTreeSet<String>,
    source_label: String,
    source: PartitionDataSource,
) -> Result<DetectedPartition> {
    let prefix_len = reader.len().min(4096) as usize;
    let prefix = reader.read_vec_at(0, prefix_len)?;
    let stem = file_name
        .rsplit('/')
        .next()
        .unwrap_or(file_name)
        .rsplit('\\')
        .next()
        .unwrap_or(file_name);

    let mut base_name = explicit_part_name.unwrap_or_else(|| normalize_partition_base(stem));
    let mut format = DetectedFormat::Raw;
    let mut raw_size = reader.len();
    let mut sparse_size = None;
    let mut notes = String::new();
    let mut is_super = false;

    if is_sparse_magic(&prefix) {
        let mut cur_r = reader.clone();
        if let Ok(sp) = inspect_sparse(&mut cur_r) {
            format = DetectedFormat::Sparse;
            raw_size = sp.raw_size;
            sparse_size = Some(sp.sparse_file_size);
            notes = format!(
                "Sparse v{}.{} ({} blocks × {}B, {} chunks)",
                sp.header.major_version,
                sp.header.minor_version,
                sp.header.total_blks,
                sp.header.blk_sz,
                sp.header.total_chunks
            );
        }
        if let Ok(img) = ImageRandomReader::open(reader.clone()) {
            if has_super_magic(&img) {
                is_super = true;
                base_name = "super".to_string();
                notes.push_str(" • Dynamic Super LP");
            }
        }
    } else if let Some(boot) = inspect_boot_header(&prefix) {
        format = match boot.kind {
            BootImageKind::VendorBoot => DetectedFormat::VendorBoot,
            _ => DetectedFormat::AndroidBoot,
        };
        if boot.kind == BootImageKind::InitBoot && base_name == "boot" {
            // keep explicit filename unless generic
        } else if stem == "unknown.img" && boot.kind == BootImageKind::InitBoot {
            base_name = "init_boot".to_string();
        }
        notes = format!(
            "Boot header v{} (kernel {}B, ramdisk {}B)",
            boot.header_version, boot.kernel_size, boot.ramdisk_size
        );
    } else if let Some(vb) = inspect_vbmeta(&prefix) {
        format = DetectedFormat::AvbVbmeta;
        notes = format!(
            "AVB {}.{} (flags=0x{:02x})",
            vb.avb_major, vb.avb_minor, vb.flags
        );
    } else if reader.len() >= 0x4000 {
        if let Ok(img) = ImageRandomReader::open(reader.clone()) {
            if has_super_magic(&img) {
                is_super = true;
                base_name = "super".to_string();
                notes = "Raw Dynamic Super LP".to_string();
            }
        }
    }

    let is_vbmeta = format == DetectedFormat::AvbVbmeta || base_name.starts_with("vbmeta");
    let (target_mode, is_slot_ab) = classify_partition_name(&base_name, dynamic_set);

    Ok(DetectedPartition {
        id: format!("{base_name}:{source_label}"),
        partition_name: base_name,
        source_label,
        raw_size,
        sparse_size,
        format,
        target_mode,
        is_slot_ab,
        is_vbmeta,
        is_super,
        selected: true,
        sha256_hex: None,
        notes,
        source,
    })
}

fn parse_flash_script_directives(
    text: &str,
) -> (Vec<(String, String)>, Vec<String>, Option<String>) {
    let mut order = Vec::new();
    let mut stripped_locks = Vec::new();
    let mut codename_hint = None;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("::") || line.starts_with("REM ") {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.contains("oem lock") || lower.contains("flashing lock") {
            stripped_locks.push(line.to_string());
            continue;
        }
        if lower.contains("fastboot") && lower.contains("flash ") {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            if let Some(idx) = tokens.iter().position(|&t| t == "flash") {
                if idx + 2 < tokens.len() {
                    let part = tokens[idx + 1]
                        .trim_matches('"')
                        .trim_start_matches('$')
                        .to_string();
                    let img = tokens[idx + 2]
                        .trim_matches('"')
                        .replace("%~dp0", "")
                        .replace("`dirname $0`/", "")
                        .replace('\\', "/");
                    order.push((part, img));
                }
            }
        }
        if let Some(rest) = line.strip_prefix("board=") {
            codename_hint = Some(rest.trim().to_string());
        }
    }
    (order, stripped_locks, codename_hint)
}

fn parse_fastboot_info_txt(text: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.len() >= 2 && tokens[0] == "flash" {
            let part = tokens[1].to_string();
            let img = tokens
                .get(2)
                .map(|s| (*s).to_string())
                .unwrap_or_else(|| format!("{part}.img"));
            pairs.push((part, img));
        }
    }
    pairs
}

fn detect_from_payload_reader(
    package_name: String,
    total_input_bytes: u64,
    payload_reader: SourceReader,
    kind: RomPackageKind,
    sideload_meta: Option<SideloadMetadata>,
    sideload_archive: Option<SourceReader>,
) -> Result<DetectedRomPackage> {
    let parsed = Arc::new(parse_payload(&mut payload_reader.clone())?);
    let mut dynamic_set = BTreeSet::new();
    for g in &parsed.dynamic_groups {
        for p in &g.partition_names {
            dynamic_set.insert(p.clone());
        }
    }

    let mut warnings = Vec::new();
    if parsed.is_incremental {
        warnings.push(
            "Incremental (Delta) payload detected: Fastboot flash cannot apply delta ops; use ADB Sideload mode."
                .into(),
        );
    }
    let codename_hint = sideload_meta.as_ref().and_then(|m| m.pre_device.clone());

    let mut partitions = Vec::with_capacity(parsed.partitions.len());
    for p in &parsed.partitions {
        let is_dyn = p.is_dynamic || dynamic_set.contains(&p.name);
        let (target_mode, is_slot_ab) = if parsed.is_incremental {
            (PartitionTargetMode::SideloadOnly, !is_dyn)
        } else if is_dyn {
            (PartitionTargetMode::FastbootdLogical, true)
        } else {
            classify_partition_name(&p.name, &dynamic_set)
        };
        let is_vbmeta = p.name.starts_with("vbmeta");
        partitions.push(DetectedPartition {
            id: format!("payload:{}", p.name),
            partition_name: p.name.clone(),
            source_label: format!("payload.bin -> {}", p.name),
            raw_size: p.size,
            sparse_size: None,
            format: DetectedFormat::PayloadEncoded,
            target_mode,
            is_slot_ab,
            is_vbmeta,
            is_super: false,
            selected: true,
            sha256_hex: p.sha256_hex.clone(),
            notes: format!("{} ops [{}]", p.op_count, p.op_summary),
            source: PartitionDataSource::PayloadPartition {
                payload_reader: payload_reader.clone(),
                parsed: Arc::clone(&parsed),
                partition_name: p.name.clone(),
            },
        });
    }

    Ok(DetectedRomPackage {
        package_name,
        kind,
        total_input_bytes,
        codename_hint,
        is_incremental_ota: parsed.is_incremental,
        partitions,
        super_info: None,
        payload_info: Some(parsed),
        sideload_meta,
        sideload_archive,
        stripped_lock_commands: Vec::new(),
        warnings,
        script_flash_order: Vec::new(),
    })
}

fn detect_from_zip(package_name: String, reader: SourceReader) -> Result<DetectedRomPackage> {
    let total_input_bytes = reader.len();
    let sideload_meta = inspect_sideload_zip(reader.clone())?;
    let mut archive = ZipArchive::new(reader.clone())?;

    let mut entry_names = Vec::with_capacity(archive.len());
    let mut payload_entry: Option<(CompressionMethod, u64, u64)> = None;
    let mut fastboot_info_txt = None;
    let mut android_info_txt = None;
    let mut flash_scripts = Vec::new();
    let mut img_entries = Vec::new();

    for i in 0..archive.len() {
        let mut f = archive.by_index(i)?;
        if f.is_dir() {
            continue;
        }
        let name = f.name().to_string();
        let lower = name.to_ascii_lowercase();
        entry_names.push(name.clone());

        if lower == "payload.bin" || lower.ends_with("/payload.bin") {
            payload_entry = Some((f.compression(), f.data_start(), f.size()));
        } else if lower == "fastboot-info.txt" || lower.ends_with("/fastboot-info.txt") {
            let mut s = String::new();
            let _ = f.read_to_string(&mut s);
            fastboot_info_txt = Some(s);
        } else if lower == "android-info.txt" || lower.ends_with("/android-info.txt") {
            let mut s = String::new();
            let _ = f.read_to_string(&mut s);
            android_info_txt = Some(s);
        } else if (lower.ends_with("flash_all.sh")
            || lower.ends_with("flash_all.bat")
            || lower.ends_with("flash-all.sh")
            || lower.ends_with("flash-all.bat"))
            && !lower.contains("lock")
        {
            let mut s = String::new();
            let _ = f.read_to_string(&mut s);
            flash_scripts.push(s);
        } else if lower.ends_with(".img") || lower.ends_with(".bin") {
            if !lower.contains("meta-inf/") {
                img_entries.push((name, f.compression(), f.data_start(), f.size()));
            }
        }
    }

    // 1. Payload OTA ZIP
    if let Some((comp, data_start, size)) = payload_entry {
        let payload_reader = if comp == CompressionMethod::Stored {
            reader.slice(data_start, size)?
        } else {
            let payload_name = entry_names
                .iter()
                .find(|n| n.to_ascii_lowercase().ends_with("payload.bin"))
                .cloned()
                .unwrap();
            let mut f = archive.by_name(&payload_name)?;
            let mut bytes = Vec::with_capacity(size as usize);
            f.read_to_end(&mut bytes)?;
            SourceReader::from_bytes(bytes)
        };
        return detect_from_payload_reader(
            package_name,
            total_input_bytes,
            payload_reader,
            RomPackageKind::PayloadOtaZip,
            sideload_meta,
            Some(reader),
        );
    }

    // 2. Fastboot ROM ZIP (has fastboot-info.txt, flash scripts, or multiple .img files)
    if !img_entries.is_empty() && (sideload_meta.is_none() || !flash_scripts.is_empty() || fastboot_info_txt.is_some()) {
        let mut script_order = Vec::new();
        let mut stripped_locks = Vec::new();
        let mut codename_hint = None;

        if let Some(fi) = &fastboot_info_txt {
            script_order.extend(parse_fastboot_info_txt(fi));
        }
        if let Some(ai) = &android_info_txt {
            let (_, _, ch) = parse_flash_script_directives(ai);
            if ch.is_some() {
                codename_hint = ch;
            }
        }
        for s in &flash_scripts {
            let (ord, locks, ch) = parse_flash_script_directives(s);
            if script_order.is_empty() {
                script_order = ord;
            }
            stripped_locks.extend(locks);
            if codename_hint.is_none() {
                codename_hint = ch;
            }
        }

        let explicit_map: BTreeMap<String, String> = script_order
            .iter()
            .map(|(part, img)| {
                let fname = img.rsplit('/').next().unwrap_or(img).to_ascii_lowercase();
                (fname, part.clone())
            })
            .collect();

        let dynamic_set = BTreeSet::new();
        let mut partitions = Vec::new();
        let mut super_info = None;

        for (entry_name, comp, data_start, size) in img_entries {
            let entry_reader = if comp == CompressionMethod::Stored {
                reader.slice(data_start, size)?
            } else {
                let mut f = archive.by_name(&entry_name)?;
                let mut bytes = Vec::with_capacity(size as usize);
                f.read_to_end(&mut bytes)?;
                SourceReader::from_bytes(bytes)
            };

            let short = entry_name
                .rsplit('/')
                .next()
                .unwrap_or(&entry_name)
                .to_ascii_lowercase();
            let explicit_part = explicit_map.get(&short).cloned();

            let part = probe_image_reader(
                &entry_name,
                explicit_part,
                entry_reader.clone(),
                &dynamic_set,
                entry_name.clone(),
                PartitionDataSource::ZipEntry {
                    archive: reader.clone(),
                    entry_name: entry_name.clone(),
                },
            )?;
            if part.is_super && super_info.is_none() {
                if let Ok(si) = inspect_super(entry_reader) {
                    super_info = Some(si);
                }
            }
            partitions.push(part);
        }

        let mut warnings = Vec::new();
        if !stripped_locks.is_empty() {
            warnings.push(format!(
                "Safety filter removed {} bootloader lock command(s) from OEM script.",
                stripped_locks.len()
            ));
        }

        return Ok(DetectedRomPackage {
            package_name,
            kind: RomPackageKind::FastbootZip,
            total_input_bytes,
            codename_hint,
            is_incremental_ota: false,
            partitions,
            super_info,
            payload_info: None,
            sideload_meta,
            sideload_archive: None,
            stripped_lock_commands: stripped_locks,
            warnings,
            script_flash_order: script_order,
        });
    }

    // 3. Recovery Sideload ZIP
    if let Some(meta) = sideload_meta {
        let mut partitions = Vec::new();
        partitions.push(DetectedPartition {
            id: format!("sideload:{package_name}"),
            partition_name: "ota_package".into(),
            source_label: package_name.clone(),
            raw_size: total_input_bytes,
            sparse_size: None,
            format: DetectedFormat::SideloadPackage,
            target_mode: PartitionTargetMode::SideloadOnly,
            is_slot_ab: false,
            is_vbmeta: false,
            is_super: false,
            selected: true,
            sha256_hex: None,
            notes: format!(
                "OTA type: {} • target device: {}",
                meta.ota_type.as_deref().unwrap_or("BLOCK"),
                meta.pre_device.as_deref().unwrap_or("any")
            ),
            source: PartitionDataSource::Direct(reader.clone()),
        });

        return Ok(DetectedRomPackage {
            package_name,
            kind: RomPackageKind::RecoverySideloadZip,
            total_input_bytes,
            codename_hint: meta.pre_device.clone(),
            is_incremental_ota: false,
            partitions,
            super_info: None,
            payload_info: None,
            sideload_meta: Some(meta),
            sideload_archive: Some(reader),
            stripped_lock_commands: Vec::new(),
            warnings: Vec::new(),
            script_flash_order: Vec::new(),
        });
    }

    Err(FlashError::InvalidFormat(
        "ZIP archive does not contain payload.bin, fastboot images, or Android OTA metadata".into(),
    ))
}

pub fn detect_from_reader(name: String, reader: SourceReader) -> Result<DetectedRomPackage> {
    if reader.len() < 4 {
        return Err(FlashError::InvalidFormat(
            "Input file too small to detect ROM format".into(),
        ));
    }
    let prefix = reader.read_vec_at(0, reader.len().min(4096) as usize)?;

    // ZIP archive (PK\x03\x04)
    if prefix.starts_with(b"PK\x03\x04") {
        return detect_from_zip(name, reader);
    }

    // Standalone payload.bin (CrAU)
    if is_payload_magic(&prefix) {
        return detect_from_payload_reader(
            name,
            reader.len(),
            reader,
            RomPackageKind::StandalonePayloadBin,
            None,
            None,
        );
    }

    // Check if super.img (Raw or Sparse LP Metadata)
    if let Ok(img_reader) = ImageRandomReader::open(reader.clone()) {
        if has_super_magic(&img_reader) {
            let super_info = inspect_super_reader(&img_reader)?;
            let mut partitions = Vec::new();
            let mut dyn_set = BTreeSet::new();
            for lp in &super_info.partitions {
                dyn_set.insert(lp.name.clone());
                partitions.push(DetectedPartition {
                    id: format!("super_lp:{}", lp.name),
                    partition_name: lp.name.clone(),
                    source_label: format!("{name} -> {}", lp.name),
                    raw_size: lp.size_bytes,
                    sparse_size: None,
                    format: DetectedFormat::SuperLogical,
                    target_mode: PartitionTargetMode::FastbootdLogical,
                    is_slot_ab: false, // already carries _a / _b when A/B
                    is_vbmeta: false,
                    is_super: false,
                    selected: true,
                    sha256_hex: None,
                    notes: format!("Group '{}' • {} extent(s)", lp.group_name, lp.extents.len()),
                    source: PartitionDataSource::SuperLogicalPartition {
                        super_reader: reader.clone(),
                        partition_name: lp.name.clone(),
                    },
                });
            }

            return Ok(DetectedRomPackage {
                package_name: name,
                kind: RomPackageKind::StandaloneSuperImage,
                total_input_bytes: reader.len(),
                codename_hint: None,
                is_incremental_ota: false,
                partitions,
                super_info: Some(super_info),
                payload_info: None,
                sideload_meta: None,
                sideload_archive: None,
                stripped_lock_commands: Vec::new(),
                warnings: Vec::new(),
                script_flash_order: Vec::new(),
            });
        }
    }

    // Single partition image
    let dynamic_set = BTreeSet::new();
    let part = probe_image_reader(
        &name,
        None,
        reader.clone(),
        &dynamic_set,
        name.clone(),
        PartitionDataSource::Direct(reader.clone()),
    )?;
    Ok(DetectedRomPackage {
        package_name: name,
        kind: RomPackageKind::SinglePartitionImage,
        total_input_bytes: reader.len(),
        codename_hint: None,
        is_incremental_ota: false,
        partitions: vec![part],
        super_info: None,
        payload_info: None,
        sideload_meta: None,
        sideload_archive: None,
        stripped_lock_commands: Vec::new(),
        warnings: Vec::new(),
        script_flash_order: Vec::new(),
    })
}

pub fn detect_from_bytes(name: impl Into<String>, bytes: Vec<u8>) -> Result<DetectedRomPackage> {
    detect_from_reader(name.into(), SourceReader::from_bytes(bytes))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn detect_from_path(path: impl AsRef<Path>) -> Result<DetectedRomPackage> {
    let path = path.as_ref();
    if path.is_file() {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("rom")
            .to_string();
        let source = Arc::new(FileSource::open(path)?);
        return detect_from_reader(name, SourceReader::new(source));
    }

    if !path.is_dir() {
        return Err(FlashError::InvalidFormat(format!(
            "Path does not exist: {}",
            path.display()
        )));
    }

    let pkg_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("rom_dir")
        .to_string();

    let mut script_order = Vec::new();
    let mut stripped_locks = Vec::new();
    let mut codename_hint = None;

    for info_name in ["fastboot-info.txt", "android-info.txt"] {
        let p = path.join(info_name);
        if p.is_file() {
            if let Ok(txt) = std::fs::read_to_string(&p) {
                if info_name == "fastboot-info.txt" {
                    script_order.extend(parse_fastboot_info_txt(&txt));
                } else {
                    let (_, _, ch) = parse_flash_script_directives(&txt);
                    if ch.is_some() {
                        codename_hint = ch;
                    }
                }
            }
        }
    }

    for script_name in ["flash_all.sh", "flash_all.bat", "flash-all.sh", "flash-all.bat"] {
        let p = path.join(script_name);
        if p.is_file() {
            if let Ok(txt) = std::fs::read_to_string(&p) {
                let (ord, locks, ch) = parse_flash_script_directives(&txt);
                if script_order.is_empty() {
                    script_order = ord;
                }
                stripped_locks.extend(locks);
                if codename_hint.is_none() {
                    codename_hint = ch;
                }
            }
        }
    }

    let explicit_map: BTreeMap<String, String> = script_order
        .iter()
        .map(|(part, img)| {
            let fname = img.rsplit('/').next().unwrap_or(img).to_ascii_lowercase();
            (fname, part.clone())
        })
        .collect();

    let mut scan_dirs = vec![path.to_path_buf()];
    let images_sub = path.join("images");
    if images_sub.is_dir() {
        scan_dirs.push(images_sub);
    }

    let mut partitions = Vec::new();
    let mut total_input_bytes = 0u64;
    let mut super_info = None;
    let dynamic_set = BTreeSet::new();

    for dir in scan_dirs {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        entries.sort();

        for file_path in entries {
            let fname = file_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            let lower = fname.to_ascii_lowercase();
            if lower == "payload.bin" {
                let src = Arc::new(FileSource::open(&file_path)?);
                return detect_from_reader(fname.to_string(), SourceReader::new(src));
            }
            if !(lower.ends_with(".img") || lower.ends_with(".bin")) {
                continue;
            }
            let src = Arc::new(FileSource::open(&file_path)?);
            let reader = SourceReader::new(src);
            total_input_bytes += reader.len();

            let explicit_part = explicit_map.get(&lower).cloned();
            let part = probe_image_reader(
                fname,
                explicit_part,
                reader.clone(),
                &dynamic_set,
                file_path.display().to_string(),
                PartitionDataSource::Direct(reader.clone()),
            )?;
            if part.is_super && super_info.is_none() {
                if let Ok(si) = inspect_super(reader) {
                    super_info = Some(si);
                }
            }
            partitions.push(part);
        }
    }

    if partitions.is_empty() {
        return Err(FlashError::InvalidFormat(format!(
            "No flashable .img or payload.bin files found in {}",
            path.display()
        )));
    }

    let mut warnings = Vec::new();
    if !stripped_locks.is_empty() {
        warnings.push(format!(
            "Safety filter removed {} bootloader lock command(s) from OEM script.",
            stripped_locks.len()
        ));
    }

    Ok(DetectedRomPackage {
        package_name: pkg_name,
        kind: RomPackageKind::FastbootDirectory,
        total_input_bytes,
        codename_hint,
        is_incremental_ota: false,
        partitions,
        super_info,
        payload_info: None,
        sideload_meta: None,
        sideload_archive: None,
        stripped_lock_commands: stripped_locks,
        warnings,
        script_flash_order: script_order,
    })
}

pub fn format_bytes_human(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GiB", b / GB)
    } else if b >= MB {
        format!("{:.2} MiB", b / MB)
    } else if b >= KB {
        format!("{:.1} KiB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Convenience helper for verifying cursor read compatibility in tests.
pub fn read_bytes_cursor(data: &[u8]) -> Cursor<&[u8]> {
    Cursor::new(data)
}
