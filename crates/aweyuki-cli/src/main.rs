mod native_transport;

use anyhow::{bail, Context, Result};
use aweyuki_core::boot_avb::{inspect_vbmeta, patch_vbmeta_verity_flags};
use aweyuki_core::demo_samples::{generate_sample_rom, SampleKind};
use aweyuki_core::detector::{detect_from_path, format_bytes_human, DetectedRomPackage};
use aweyuki_core::payload::{extract_payload_partition, parse_payload};
use aweyuki_core::planner::{
    build_flash_plan, export_script, DeviceTargetInfo, FlashOptions, ScriptFormat, SlotSelection,
    SuperFlashMode,
};
use aweyuki_core::reader::{FileSource, SourceReader};
use aweyuki_core::simulator::{
    execute_flash_plan, DeviceSimulator, FlashTransport, SimulatorProfile,
};
use aweyuki_core::sparse::auto_split_image;
use aweyuki_core::super_lp::{inspect_super, unpack_super_partition};
use clap::{Parser, Subcommand, ValueEnum};
use native_transport::{discover_real_devices, query_termux_usb_devices, NativeCliTransport};
use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(
    name = "aweyuki",
    version,
    about = "Aweyuki Flasher CLI — Auto-detect partitions, sparse splitter, payload.bin, super.img & adb sideload (Linux/macOS/Windows/Termux)"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, ValueEnum)]
enum SlotArg {
    Active,
    A,
    B,
    Both,
}

impl From<SlotArg> for SlotSelection {
    fn from(s: SlotArg) -> Self {
        match s {
            SlotArg::Active => Self::Active,
            SlotArg::A => Self::SlotA,
            SlotArg::B => Self::SlotB,
            SlotArg::Both => Self::BothSlots,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum SuperModeArg {
    Whole,
    Unpack,
}

impl From<SuperModeArg> for SuperFlashMode {
    fn from(s: SuperModeArg) -> Self {
        match s {
            SuperModeArg::Whole => Self::WholeSparse,
            SuperModeArg::Unpack => Self::UnpackToFastbootd,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum SimProfileArg {
    Pixel9,
    Xiaomi,
    LowBuffer,
    Sideload,
}

impl From<SimProfileArg> for SimulatorProfile {
    fn from(p: SimProfileArg) -> Self {
        match p {
            SimProfileArg::Pixel9 => Self::Pixel9ProAB,
            SimProfileArg::Xiaomi => Self::Xiaomi14TProHyperOS,
            SimProfileArg::LowBuffer => Self::LowBufferOtg256K,
            SimProfileArg::Sideload => Self::RecoverySideloadDevice,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ScriptFormatArg {
    Sh,
    Bat,
    Termux,
}

impl From<ScriptFormatArg> for ScriptFormat {
    fn from(f: ScriptFormatArg) -> Self {
        match f {
            ScriptFormatArg::Sh => Self::Bash,
            ScriptFormatArg::Bat => Self::WindowsBatch,
            ScriptFormatArg::Termux => Self::TermuxOtg,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum SampleKindArg {
    PayloadZip,
    Payload,
    Super,
    FastbootZip,
    SideloadZip,
}

impl From<SampleKindArg> for SampleKind {
    fn from(k: SampleKindArg) -> Self {
        match k {
            SampleKindArg::PayloadZip => Self::PayloadOtaZip,
            SampleKindArg::Payload => Self::StandalonePayload,
            SampleKindArg::Super => Self::SuperSparse,
            SampleKindArg::FastbootZip => Self::FastbootZip,
            SampleKindArg::SideloadZip => Self::RecoverySideloadZip,
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    /// List connected Fastboot, ADB, Sideload, and Termux USB OTG devices
    Devices {
        /// Show built-in simulator profiles as well
        #[arg(long)]
        simulate: bool,
    },

    /// Auto-detect ROM package type, magic headers, partitions, and flash plan
    Inspect {
        input: PathBuf,
        #[arg(long, default_value = "256M")]
        max_size: String,
        #[arg(long, value_enum, default_value = "whole")]
        super_mode: SuperModeArg,
    },

    /// Flash a ROM (.zip, payload.bin, super.img, .img, or directory)
    Flash {
        input: PathBuf,
        #[arg(short, long)]
        serial: Option<String>,
        #[arg(long, value_enum, default_value = "active")]
        slot: SlotArg,
        #[arg(long, default_value = "256M")]
        max_size: String,
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        disable_verity: bool,
        #[arg(long, value_enum, default_value = "whole")]
        super_mode: SuperModeArg,
        #[arg(long)]
        wipe: bool,
        #[arg(long)]
        sideload: bool,
        #[arg(long)]
        no_reboot: bool,
        #[arg(long)]
        termux_usb: bool,
        #[arg(long)]
        dry_run: bool,
        /// Run against a built-in virtual device simulator instead of USB hardware
        #[arg(long, value_enum)]
        simulate: Option<SimProfileArg>,
    },

    /// Split a raw or sparse image into multiple Android Sparse chunks <= max-size
    Split {
        image: PathBuf,
        #[arg(long, default_value = "256M")]
        max_size: String,
        #[arg(short, long, default_value = "./chunks")]
        output_dir: PathBuf,
    },

    /// Inspect or extract partitions from payload.bin or payload OTA .zip
    Payload {
        input: PathBuf,
        #[arg(long)]
        list: bool,
        /// Comma-separated partition names or "all"
        #[arg(long)]
        extract: Option<String>,
        #[arg(short, long, default_value = "./extracted_payload")]
        output_dir: PathBuf,
    },

    /// Inspect or unpack logical partitions from super.img (raw or sparse)
    Super {
        input: PathBuf,
        #[arg(long)]
        list: bool,
        /// Comma-separated logical partition names or "all"
        #[arg(long)]
        unpack: Option<String>,
        #[arg(short, long, default_value = "./unpacked_super")]
        output_dir: PathBuf,
    },

    /// Patch AVB0 flags in vbmeta.img to disable dm-verity and verification
    VbmetaPatch {
        input: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Sideload an OTA .zip via adb sideload
    Sideload {
        zip: PathBuf,
        #[arg(short, long)]
        serial: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        simulate: bool,
    },

    /// Export a standalone flash script (.sh / .bat / Termux OTG)
    ExportScript {
        input: PathBuf,
        #[arg(long, value_enum, default_value = "sh")]
        format: ScriptFormatArg,
        #[arg(long, value_enum, default_value = "active")]
        slot: SlotArg,
        #[arg(long, default_value = "256M")]
        max_size: String,
        #[arg(long, value_enum, default_value = "whole")]
        super_mode: SuperModeArg,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Generate a genuine binary sample ROM (payload, super, fastboot zip, sideload zip)
    GenerateSample {
        #[arg(long, value_enum)]
        kind: SampleKindArg,
        #[arg(short, long)]
        output: PathBuf,
    },
}

fn parse_byte_size(s: &str) -> Result<u64> {
    let trimmed = s.trim().to_ascii_uppercase();
    let trimmed = trimmed.trim_end_matches('B');
    let (num_part, mult) = if let Some(n) = trimmed.strip_suffix('G') {
        (n, 1024u64 * 1024 * 1024)
    } else if let Some(n) = trimmed.strip_suffix('M') {
        (n, 1024u64 * 1024)
    } else if let Some(n) = trimmed.strip_suffix('K') {
        (n, 1024u64)
    } else {
        (trimmed, 1u64)
    };
    let val: u64 = num_part
        .trim()
        .parse()
        .with_context(|| format!("Invalid byte size '{s}'"))?;
    Ok(val * mult)
}

fn print_package_summary(pkg: &DetectedRomPackage) {
    println!("Package : {}", pkg.package_name);
    println!("Type    : {}", pkg.kind.label());
    println!("Size    : {}", format_bytes_human(pkg.total_input_bytes));
    if let Some(c) = &pkg.codename_hint {
        println!("Device  : {c}");
    }
    for w in &pkg.warnings {
        println!("WARNING : {w}");
    }
    println!("\nDetected Partitions ({}):", pkg.partitions.len());
    println!(
        "{:<18} {:<10} {:<12} {:<10} {}",
        "PARTITION", "FORMAT", "SIZE", "STAGE", "DETAILS"
    );
    println!("{}", "-".repeat(78));
    for p in &pkg.partitions {
        let stage = match p.target_mode {
            aweyuki_core::detector::PartitionTargetMode::Bootloader => "bootloader",
            aweyuki_core::detector::PartitionTargetMode::FastbootdLogical => "fastbootd",
            aweyuki_core::detector::PartitionTargetMode::SideloadOnly => "sideload",
        };
        println!(
            "{:<18} {:<10} {:<12} {:<10} {}",
            p.partition_name,
            p.format.label(),
            format_bytes_human(p.raw_size),
            stage,
            p.notes
        );
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let is_termux = std::env::var_os("TERMUX_VERSION").is_some()
        || std::env::var("PREFIX")
            .map(|p| p.contains("com.termux"))
            .unwrap_or(false);

    match cli.command {
        Commands::Devices { simulate } => {
            let real = discover_real_devices(is_termux);
            println!("Connected USB Devices ({}):", real.len());
            for d in &real {
                println!(
                    "  [{}] {} | product={} | mode={} | slot={} | max_dl={}",
                    d.connection,
                    d.info.serial,
                    d.info.product,
                    d.info.mode.label(),
                    d.info.current_slot.as_deref().unwrap_or("none"),
                    format_bytes_human(d.info.max_download_size)
                );
            }
            if is_termux {
                let usb = query_termux_usb_devices();
                if !usb.is_empty() {
                    println!("Termux USB OTG Nodes: {}", usb.join(", "));
                }
            }
            if simulate {
                println!("\nAvailable Virtual Simulator Profiles:");
                for prof in SimulatorProfile::all() {
                    let sim = DeviceSimulator::from_profile(*prof);
                    println!(
                        "  {:<42} serial={} max_dl={}",
                        prof.label(),
                        sim.info.serial,
                        format_bytes_human(sim.info.max_download_size)
                    );
                }
            }
        }

        Commands::Inspect {
            input,
            max_size,
            super_mode,
        } => {
            let pkg = detect_from_path(&input)?;
            print_package_summary(&pkg);
            let max_dl = parse_byte_size(&max_size)?;
            let mut opts = FlashOptions::default();
            opts.max_download_size = max_dl;
            opts.super_mode = super_mode.into();
            let sim = DeviceSimulator::from_profile(SimulatorProfile::Pixel9ProAB);
            let plan = build_flash_plan(&pkg, &sim.info, &opts);
            println!("\nPlanned Execution Steps ({}):", plan.steps.len());
            for (i, st) in plan.steps.iter().enumerate() {
                println!("  {:>2}. {}", i + 1, st.summary());
            }
        }

        Commands::Flash {
            input,
            serial,
            slot,
            max_size,
            disable_verity,
            super_mode,
            wipe,
            sideload,
            no_reboot,
            termux_usb,
            dry_run,
            simulate,
        } => {
            let pkg = detect_from_path(&input)?;
            print_package_summary(&pkg);
            let max_dl = parse_byte_size(&max_size)?;
            let opts = FlashOptions {
                slot: slot.into(),
                max_download_size: max_dl,
                auto_sparse_split: true,
                disable_verity,
                disable_verification: disable_verity,
                super_mode: super_mode.into(),
                auto_resize_logical: true,
                reclaim_cow: true,
                wipe_userdata: wipe,
                force_sideload: sideload,
                reboot_after: !no_reboot,
            };

            let mut sim_transport;
            let mut real_transport;
            let transport: &mut dyn FlashTransport = if let Some(prof) = simulate {
                sim_transport = DeviceSimulator::from_profile(prof.into());
                &mut sim_transport
            } else {
                if termux_usb && std::env::var_os("TERMUX_USB_FD").is_none() {
                    if let Some(dev_node) = query_termux_usb_devices().into_iter().next() {
                        let raw_args: Vec<String> = std::env::args().skip(1).collect();
                        native_transport::run_under_termux_usb_if_requested(
                            std::path::Path::new(&dev_node),
                            &raw_args,
                        )?;
                        return Ok(());
                    }
                }
                real_transport = NativeCliTransport::detect(serial, termux_usb);
                if let Some(node) = &real_transport.termux_usb_dev {
                    println!("Termux USB OTG device node: {node}");
                }
                &mut real_transport
            };

            let device_info: DeviceTargetInfo = transport.query_device()?;
            let plan = build_flash_plan(&pkg, &device_info, &opts);
            let report = execute_flash_plan(
                &pkg,
                &plan,
                &opts,
                transport,
                dry_run,
                |_, _, _| {},
                |line| println!("{line}"),
            )?;
            println!(
                "\nDone: {} steps, {} chunks, {} transferred.",
                report.steps_completed,
                report.chunks_sent,
                format_bytes_human(report.total_bytes_transferred)
            );
        }

        Commands::Split {
            image,
            max_size,
            output_dir,
        } => {
            let max_dl = parse_byte_size(&max_size)?;
            let src = Arc::new(FileSource::open(&image)?);
            let reader = SourceReader::new(src);
            let stem = image
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("image");
            let chunks = auto_split_image(reader, max_dl)?;
            std::fs::create_dir_all(&output_dir)?;
            for ch in &chunks {
                let out_path = output_dir.join(format!("{stem}_chunk_{}.simg", ch.meta.index));
                std::fs::write(&out_path, &ch.data)?;
                println!(
                    "Wrote {} ({} | blocks {}..{}/{})",
                    out_path.display(),
                    format_bytes_human(ch.data.len() as u64),
                    ch.meta.start_block,
                    ch.meta.start_block + ch.meta.active_blocks,
                    ch.meta.total_blocks
                );
            }
        }

        Commands::Payload {
            input,
            list,
            extract,
            output_dir,
        } => {
            let pkg = detect_from_path(&input)?;
            let Some(parsed) = pkg.payload_info else {
                bail!("Input '{}' does not contain a CrAU payload.bin", input.display());
            };
            let payload_reader = if let Some(first) = pkg.partitions.first() {
                match &first.source {
                    aweyuki_core::detector::PartitionDataSource::PayloadPartition {
                        payload_reader,
                        ..
                    } => payload_reader.clone(),
                    _ => bail!("Unexpected source variant for payload"),
                }
            } else {
                let src = Arc::new(FileSource::open(&input)?);
                SourceReader::new(src)
            };
            let parsed_check = parse_payload(&mut payload_reader.clone())?;

            if list || extract.is_none() {
                println!(
                    "Payload v{} | block_size={} | incremental={}",
                    parsed.version, parsed.block_size, parsed.is_incremental
                );
                for p in &parsed.partitions {
                    println!(
                        "  {:<18} {:<12} dynamic={} ops=[{}]",
                        p.name,
                        format_bytes_human(p.size),
                        p.is_dynamic,
                        p.op_summary
                    );
                }
            }

            if let Some(target_arg) = extract {
                std::fs::create_dir_all(&output_dir)?;
                let targets: Vec<String> = if target_arg == "all" {
                    parsed_check
                        .partitions
                        .iter()
                        .map(|p| p.name.clone())
                        .collect()
                } else {
                    target_arg
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                };
                for name in targets {
                    let out_path = output_dir.join(format!("{name}.img"));
                    let mut f = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .open(&out_path)?;
                    let sz = extract_payload_partition(
                        &payload_reader,
                        &parsed_check,
                        &name,
                        &mut f,
                        |_, _| {},
                    )?;
                    println!("Extracted {name} -> {} ({})", out_path.display(), format_bytes_human(sz));
                }
            }
        }

        Commands::Super {
            input,
            list,
            unpack,
            output_dir,
        } => {
            let src = Arc::new(FileSource::open(&input)?);
            let reader = SourceReader::new(src);
            let info = inspect_super(reader.clone())?;
            if list || unpack.is_none() {
                println!(
                    "Super LP v{}.{} | sparse={} | raw_size={}",
                    info.major_version,
                    info.minor_version,
                    info.is_sparse,
                    format_bytes_human(info.raw_size)
                );
                for p in &info.partitions {
                    println!(
                        "  {:<20} {:<12} group={} extents={}",
                        p.name,
                        format_bytes_human(p.size_bytes),
                        p.group_name,
                        p.extents.len()
                    );
                }
            }
            if let Some(target_arg) = unpack {
                std::fs::create_dir_all(&output_dir)?;
                let targets: Vec<String> = if target_arg == "all" {
                    info.partitions.iter().map(|p| p.name.clone()).collect()
                } else {
                    target_arg
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                };
                for name in targets {
                    let out_path = output_dir.join(format!("{name}.img"));
                    let mut f = File::create(&out_path)?;
                    let sz = unpack_super_partition(reader.clone(), &name, &mut f)?;
                    println!("Unpacked {name} -> {} ({})", out_path.display(), format_bytes_human(sz));
                }
            }
        }

        Commands::VbmetaPatch { input, output } => {
            let mut bytes = std::fs::read(&input)?;
            let before = inspect_vbmeta(&bytes)
                .ok_or_else(|| anyhow::anyhow!("Not a valid AVB0 vbmeta image"))?;
            let new_flags = patch_vbmeta_verity_flags(&mut bytes, true, true)?;
            let dest = output.unwrap_or(input);
            std::fs::write(&dest, &bytes)?;
            println!(
                "Patched AVB0 flags: 0x{:02x} -> 0x{:02x} ({})",
                before.flags,
                new_flags,
                dest.display()
            );
        }

        Commands::Sideload {
            zip,
            serial,
            dry_run,
            simulate,
        } => {
            let pkg = detect_from_path(&zip)?;
            let mut opts = FlashOptions::default();
            opts.force_sideload = true;
            let mut sim_transport;
            let mut real_transport;
            let transport: &mut dyn FlashTransport = if simulate {
                sim_transport = DeviceSimulator::from_profile(SimulatorProfile::RecoverySideloadDevice);
                &mut sim_transport
            } else {
                real_transport = NativeCliTransport::detect(serial, false);
                &mut real_transport
            };
            let dev = transport.query_device()?;
            let plan = build_flash_plan(&pkg, &dev, &opts);
            execute_flash_plan(
                &pkg,
                &plan,
                &opts,
                transport,
                dry_run,
                |_, _, _| {},
                |line| println!("{line}"),
            )?;
        }

        Commands::ExportScript {
            input,
            format,
            slot,
            max_size,
            super_mode,
            output,
        } => {
            let pkg = detect_from_path(&input)?;
            let max_dl = parse_byte_size(&max_size)?;
            let opts = FlashOptions {
                slot: slot.into(),
                max_download_size: max_dl,
                super_mode: super_mode.into(),
                ..FlashOptions::default()
            };
            let sim = DeviceSimulator::from_profile(SimulatorProfile::Pixel9ProAB);
            let plan = build_flash_plan(&pkg, &sim.info, &opts);
            let script = export_script(&plan, format.into());
            if let Some(out_path) = output {
                std::fs::write(&out_path, &script)?;
                println!("Exported flash script to {}", out_path.display());
            } else {
                print!("{script}");
            }
        }

        Commands::GenerateSample { kind, output } => {
            let (default_name, bytes) = generate_sample_rom(kind.into())?;
            if let Some(parent) = output.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            std::fs::write(&output, &bytes)?;
            println!(
                "Generated sample '{}' ({}) -> {}",
                default_name,
                format_bytes_human(bytes.len() as u64),
                output.display()
            );
        }
    }

    Ok(())
}
