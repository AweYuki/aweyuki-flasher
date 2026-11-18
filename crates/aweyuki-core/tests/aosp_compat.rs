use aweyuki_core::boot_avb::{inspect_vbmeta, patch_vbmeta_verity_flags};
use aweyuki_core::demo_samples::{generate_sample_rom, make_patterned_fs_image, SampleKind};
use aweyuki_core::detector::{detect_from_bytes, RomPackageKind};
use aweyuki_core::payload::{extract_payload_partition_bytes, parse_payload};
use aweyuki_core::planner::{build_flash_plan, export_script, FlashOptions, ScriptFormat, SuperFlashMode};
use aweyuki_core::reader::SourceReader;
use aweyuki_core::simulator::{execute_flash_plan, DeviceSimulator, FlashTransport, SimulatorProfile};
use aweyuki_core::sparse::{resparse_chunks, sparse_to_raw, split_raw_to_sparse_chunks};
use aweyuki_core::super_lp::{build_super_image, inspect_super, unpack_super_partition_bytes, SuperInputPartition};
use std::io::Cursor;
use std::process::Command;

#[test]
fn sparse_split_and_resparse_match_aosp_simg2img() {
    if Command::new("simg2img").arg("--help").output().is_err() {
        return;
    }

    let tmp = std::env::temp_dir().join(format!("aweyuki_sparse_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    // 2 MiB patterned raw image (includes zero blocks and varied data blocks)
    let raw = make_patterned_fs_image(2 * 1024 * 1024, 0x3D);
    let max_dl = 300 * 1024u64;

    let mut cur = Cursor::new(&raw);
    let chunks = split_raw_to_sparse_chunks(&mut cur, raw.len() as u64, 4096, max_dl).unwrap();
    assert!(chunks.len() > 2, "expected multiple sparse chunks");

    let mut chunk_paths = Vec::new();
    for (idx, ch) in chunks.iter().enumerate() {
        assert!(
            ch.data.len() as u64 <= max_dl,
            "chunk {idx} exceeded max_download_size ({} > {max_dl})",
            ch.data.len()
        );
        let p = tmp.join(format!("raw_chunk_{idx}.simg"));
        std::fs::write(&p, &ch.data).unwrap();
        chunk_paths.push(p);
    }

    let reconstructed_path = tmp.join("reconstructed_from_raw_split.raw");
    let mut cmd = Command::new("simg2img");
    for p in &chunk_paths {
        cmd.arg(p);
    }
    cmd.arg(&reconstructed_path);
    let status = cmd.status().unwrap();
    assert!(status.success(), "simg2img failed on raw-split sparse chunks");
    let reconstructed = std::fs::read(&reconstructed_path).unwrap();
    assert_eq!(reconstructed.len(), raw.len());
    assert_eq!(reconstructed, raw, "simg2img output differed from original raw");

    // Now test resparse_chunks from an img2simg-created sparse image
    if Command::new("img2simg").output().is_ok() {
        let raw_path = tmp.join("orig.raw");
        let single_sparse_path = tmp.join("orig.simg");
        std::fs::write(&raw_path, &raw).unwrap();
        let st = Command::new("img2simg")
            .arg(&raw_path)
            .arg(&single_sparse_path)
            .status()
            .unwrap();
        assert!(st.success(), "img2simg failed");

        let single_sparse_bytes = std::fs::read(&single_sparse_path).unwrap();
        let mut sr = SourceReader::from_bytes(single_sparse_bytes);
        let rechunks = resparse_chunks(&mut sr, max_dl).unwrap();
        assert!(rechunks.len() > 2);

        let mut rechunk_paths = Vec::new();
        for (idx, ch) in rechunks.iter().enumerate() {
            assert!(ch.data.len() as u64 <= max_dl);
            let p = tmp.join(format!("rechunk_{idx}.simg"));
            std::fs::write(&p, &ch.data).unwrap();
            rechunk_paths.push(p);
        }

        let reconstructed2_path = tmp.join("reconstructed_from_resparse.raw");
        let mut cmd2 = Command::new("simg2img");
        for p in &rechunk_paths {
            cmd2.arg(p);
        }
        cmd2.arg(&reconstructed2_path);
        let st2 = cmd2.status().unwrap();
        assert!(st2.success(), "simg2img failed on resparsed chunks");
        let reconstructed2 = std::fs::read(&reconstructed2_path).unwrap();
        assert_eq!(reconstructed2, raw, "resparse + simg2img differed from original raw");
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn super_build_and_unpack_match_aosp_lpunpack() {
    let sys_data = make_patterned_fs_image(128 * 1024, 0x11);
    let ven_data = make_patterned_fs_image(64 * 1024, 0x22);

    let super_sparse = build_super_image(
        4 * 1024 * 1024,
        65536,
        &[
            SuperInputPartition {
                name: "system_a".into(),
                group: "main_a".into(),
                data: sys_data.clone(),
                readonly: true,
            },
            SuperInputPartition {
                name: "vendor_a".into(),
                group: "main_a".into(),
                data: ven_data.clone(),
                readonly: true,
            },
        ],
        true,
    )
    .unwrap();

    // Rust inspection & unpack directly from SPARSE super.img
    let info = inspect_super(SourceReader::from_bytes(super_sparse.clone())).unwrap();
    assert!(info.is_sparse);
    assert_eq!(info.partitions.len(), 2);

    let rust_sys =
        unpack_super_partition_bytes(SourceReader::from_bytes(super_sparse.clone()), "system_a")
            .unwrap();
    let rust_ven =
        unpack_super_partition_bytes(SourceReader::from_bytes(super_sparse.clone()), "vendor_a")
            .unwrap();
    assert_eq!(rust_sys, sys_data);
    assert_eq!(rust_ven, ven_data);

    // Cross-check against AOSP /usr/bin/lpunpack if installed
    if Command::new("lpunpack").output().is_ok() {
        let tmp = std::env::temp_dir().join(format!("aweyuki_lp_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let mut super_raw = Vec::new();
        sparse_to_raw(&mut SourceReader::from_bytes(super_sparse), &mut super_raw).unwrap();
        let super_raw_path = tmp.join("super.raw.img");
        std::fs::write(&super_raw_path, &super_raw).unwrap();

        let out_dir = tmp.join("unpacked");
        std::fs::create_dir_all(&out_dir).unwrap();
        let st = Command::new("lpunpack")
            .arg(&super_raw_path)
            .arg(&out_dir)
            .status()
            .unwrap();
        assert!(st.success(), "AOSP lpunpack rejected our generated super.img");

        assert_eq!(std::fs::read(out_dir.join("system_a.img")).unwrap(), sys_data);
        assert_eq!(std::fs::read(out_dir.join("vendor_a.img")).unwrap(), ven_data);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

#[test]
fn payload_roundtrip_vbmeta_patch_and_full_simulation() {
    let (name, bytes) = generate_sample_rom(SampleKind::PayloadOtaZip).unwrap();
    let pkg = detect_from_bytes(name, bytes).unwrap();
    assert_eq!(pkg.kind, RomPackageKind::PayloadOtaZip);
    assert!(pkg.partitions.len() >= 6);

    // Extract boot and vbmeta from payload inside ZIP
    let payload_part = pkg.partitions.iter().find(|p| p.partition_name == "vbmeta").unwrap();
    let mut vb_bytes = payload_part.source.materialize_bytes().unwrap();
    let vb_before = inspect_vbmeta(&vb_bytes).unwrap();
    assert_eq!(vb_before.flags, 0);
    let patched_flags = patch_vbmeta_verity_flags(&mut vb_bytes, true, true).unwrap();
    assert_eq!(patched_flags, 3);
    let vb_after = inspect_vbmeta(&vb_bytes).unwrap();
    assert!(vb_after.hashtree_disabled);
    assert!(vb_after.verification_disabled);

    // Run full flash execution against LowBufferOtg256K simulator (forces chunk splitting!)
    let mut sim = DeviceSimulator::from_profile(SimulatorProfile::LowBufferOtg256K);
    let dev = sim.query_device().unwrap();
    let mut opts = FlashOptions::default();
    opts.max_download_size = 256 * 1024;
    let plan = build_flash_plan(&pkg, &dev, &opts);
    let termux_sh = export_script(&plan, ScriptFormat::TermuxOtg);
    assert!(termux_sh.contains("termux-usb"));

    let mut logs = Vec::new();
    let report = execute_flash_plan(
        &pkg,
        &plan,
        &opts,
        &mut sim,
        false,
        |_, _, _| {},
        |line| logs.push(line.to_string()),
    )
    .unwrap();
    let flash_step_count = plan
        .steps
        .iter()
        .filter(|s| matches!(s, aweyuki_core::planner::FlashStep::FlashPartition { .. }))
        .count();
    assert!(
        report.chunks_sent > flash_step_count,
        "expected sparse splitting to produce more chunks ({}) than flash steps ({})",
        report.chunks_sent,
        flash_step_count
    );
    assert!(logs.iter().any(|l| l.contains("AVB0 vbmeta patched")));
    assert!(logs.iter().any(|l| l.contains("Split 'system_a'")));

    // Also test standalone payload.bin + FastbootZip unpack-to-fastbootd + Sideload
    let (_pname, pbytes) = generate_sample_rom(SampleKind::StandalonePayload).unwrap();
    let mut pr = SourceReader::from_bytes(pbytes);
    let parsed = parse_payload(&mut pr).unwrap();
    let sys_extracted = extract_payload_partition_bytes(&pr, &parsed, "system").unwrap();
    assert_eq!(sys_extracted.len(), 512 * 1024);

    let (fname, fbytes) = generate_sample_rom(SampleKind::FastbootZip).unwrap();
    let fpkg = detect_from_bytes(fname, fbytes).unwrap();
    assert_eq!(fpkg.kind, RomPackageKind::FastbootZip);
    assert_eq!(fpkg.stripped_lock_commands.len(), 1);
    opts.super_mode = SuperFlashMode::UnpackToFastbootd;
    let fplan = build_flash_plan(&fpkg, &dev, &opts);
    assert!(fplan
        .steps
        .iter()
        .any(|s| matches!(s, aweyuki_core::planner::FlashStep::RebootFastbootd)));

    let (sname, sbytes) = generate_sample_rom(SampleKind::RecoverySideloadZip).unwrap();
    let spkg = detect_from_bytes(sname, sbytes).unwrap();
    assert_eq!(spkg.kind, RomPackageKind::RecoverySideloadZip);
    let mut rsim = DeviceSimulator::from_profile(SimulatorProfile::RecoverySideloadDevice);
    let rdev = rsim.query_device().unwrap();
    let splan = build_flash_plan(&spkg, &rdev, &opts);
    execute_flash_plan(&spkg, &splan, &opts, &mut rsim, false, |_, _, _| {}, |_| {}).unwrap();
    assert_eq!(rsim.last_sideloaded.as_deref(), Some("aweyuki-recovery-sideload.zip"));
}
