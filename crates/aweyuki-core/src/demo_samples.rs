use crate::boot_avb::{ANDROID_BOOT_MAGIC, AVB0_MAGIC, VENDOR_BOOT_MAGIC};
use crate::error::Result;
use crate::payload::{build_payload_bytes, PayloadBuildPartition};
use crate::sideload::build_sample_sideload_zip;
use crate::super_lp::{build_super_image, SuperInputPartition};
use byteorder::{BigEndian, LittleEndian, WriteBytesExt};
use std::io::{Cursor, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    PayloadOtaZip,
    StandalonePayload,
    SuperSparse,
    FastbootZip,
    RecoverySideloadZip,
}

impl SampleKind {
    pub fn filename(&self) -> &'static str {
        match self {
            Self::PayloadOtaZip => "aweyuki-ota-payload.zip",
            Self::StandalonePayload => "payload.bin",
            Self::SuperSparse => "super.img",
            Self::FastbootZip => "aweyuki-fastboot-rom.zip",
            Self::RecoverySideloadZip => "aweyuki-recovery-sideload.zip",
        }
    }
}

pub fn make_boot_img(size: usize, is_init_boot: bool) -> Vec<u8> {
    let mut buf = vec![0u8; size.max(4096)];
    buf[0..8].copy_from_slice(ANDROID_BOOT_MAGIC);
    let mut cur = Cursor::new(&mut buf[8..44]);
    if is_init_boot {
        cur.write_u32::<LittleEndian>(0).unwrap(); // kernel_size = 0
        cur.write_u32::<LittleEndian>(0).unwrap();
        cur.write_u32::<LittleEndian>(65536).unwrap(); // ramdisk_size
    } else {
        cur.write_u32::<LittleEndian>(131072).unwrap(); // kernel_size
        cur.write_u32::<LittleEndian>(0).unwrap();
        cur.write_u32::<LittleEndian>(32768).unwrap(); // ramdisk_size
    }
    cur.write_u32::<LittleEndian>(0).unwrap();
    cur.write_u32::<LittleEndian>(0).unwrap();
    cur.write_u32::<LittleEndian>(0).unwrap();
    cur.write_u32::<LittleEndian>(0).unwrap();
    cur.write_u32::<LittleEndian>(4096).unwrap(); // page_size
    cur.write_u32::<LittleEndian>(4).unwrap(); // header_version = 4
    // Add recognizable patterned payload data after header page
    for (i, b) in buf[4096..].iter_mut().enumerate() {
        *b = ((i * 17 + if is_init_boot { 3 } else { 11 }) & 0xFF) as u8;
    }
    buf
}

pub fn make_vendor_boot_img(size: usize) -> Vec<u8> {
    let mut buf = vec![0u8; size.max(4096)];
    buf[0..8].copy_from_slice(VENDOR_BOOT_MAGIC);
    let mut cur = Cursor::new(&mut buf[8..28]);
    cur.write_u32::<LittleEndian>(4).unwrap(); // header_version
    cur.write_u32::<LittleEndian>(4096).unwrap(); // page_size
    cur.write_u32::<LittleEndian>(0).unwrap();
    cur.write_u32::<LittleEndian>(0).unwrap();
    cur.write_u32::<LittleEndian>(24576).unwrap(); // ramdisk_size
    for (i, b) in buf[4096..].iter_mut().enumerate() {
        *b = ((i * 29 + 7) & 0xFF) as u8;
    }
    buf
}

pub fn make_vbmeta_img(flags: u32) -> Vec<u8> {
    let mut buf = vec![0u8; 4096];
    buf[0..4].copy_from_slice(AVB0_MAGIC);
    (&mut buf[4..8]).write_u32::<BigEndian>(1).unwrap();
    (&mut buf[8..12]).write_u32::<BigEndian>(2).unwrap();
    (&mut buf[120..124]).write_u32::<BigEndian>(flags).unwrap();
    buf[128..143].copy_from_slice(b"avbtool 1.3.0\0\0");
    buf
}

/// Patterned filesystem-like blob with both non-zero data blocks and zero holes
/// so sparse splitting visibly produces both RAW and FILL(0) chunks.
pub fn make_patterned_fs_image(total_bytes: usize, seed: u8) -> Vec<u8> {
    let mut buf = vec![0u8; total_bytes];
    let block = 4096usize;
    let num_blocks = total_bytes / block;
    for blk_idx in 0..num_blocks {
        // Leave every 3rd quarter zeroed to demonstrate sparse zero compression
        if (blk_idx % 8) >= 6 {
            continue;
        }
        let start = blk_idx * block;
        for (j, b) in buf[start..start + block].iter_mut().enumerate() {
            *b = seed
                .wrapping_add((blk_idx as u8).wrapping_mul(31))
                .wrapping_add((j as u8).wrapping_mul(13));
        }
    }
    buf
}

fn build_demo_payload() -> Result<Vec<u8>> {
    let partitions = vec![
        PayloadBuildPartition {
            name: "vbmeta".into(),
            data: make_vbmeta_img(0),
            compress_xz: false,
            dynamic_group: None,
        },
        PayloadBuildPartition {
            name: "boot".into(),
            data: make_boot_img(64 * 1024, false),
            compress_xz: true,
            dynamic_group: None,
        },
        PayloadBuildPartition {
            name: "init_boot".into(),
            data: make_boot_img(48 * 1024, true),
            compress_xz: true,
            dynamic_group: None,
        },
        PayloadBuildPartition {
            name: "vendor_boot".into(),
            data: make_vendor_boot_img(48 * 1024),
            compress_xz: true,
            dynamic_group: None,
        },
        PayloadBuildPartition {
            name: "dtbo".into(),
            data: make_patterned_fs_image(32 * 1024, 0x33),
            compress_xz: false,
            dynamic_group: None,
        },
        PayloadBuildPartition {
            name: "system".into(),
            data: make_patterned_fs_image(512 * 1024, 0x55),
            compress_xz: true,
            dynamic_group: Some("qti_dynamic_partitions".into()),
        },
        PayloadBuildPartition {
            name: "vendor".into(),
            data: make_patterned_fs_image(256 * 1024, 0x77),
            compress_xz: true,
            dynamic_group: Some("qti_dynamic_partitions".into()),
        },
        PayloadBuildPartition {
            name: "product".into(),
            data: make_patterned_fs_image(192 * 1024, 0x99),
            compress_xz: false,
            dynamic_group: Some("qti_dynamic_partitions".into()),
        },
    ];
    build_payload_bytes(&partitions, 4096)
}

fn build_demo_super_sparse() -> Result<Vec<u8>> {
    let parts = vec![
        SuperInputPartition {
            name: "system_a".into(),
            group: "qti_dynamic_partitions_a".into(),
            data: make_patterned_fs_image(512 * 1024, 0x41),
            readonly: true,
        },
        SuperInputPartition {
            name: "vendor_a".into(),
            group: "qti_dynamic_partitions_a".into(),
            data: make_patterned_fs_image(256 * 1024, 0x62),
            readonly: true,
        },
        SuperInputPartition {
            name: "product_a".into(),
            group: "qti_dynamic_partitions_a".into(),
            data: make_patterned_fs_image(192 * 1024, 0x83),
            readonly: true,
        },
    ];
    build_super_image(4 * 1024 * 1024, 65536, &parts, true)
}

pub fn generate_sample_rom(kind: SampleKind) -> Result<(String, Vec<u8>)> {
    let name = kind.filename().to_string();
    let bytes = match kind {
        SampleKind::StandalonePayload => build_demo_payload()?,
        SampleKind::PayloadOtaZip => {
            let payload = build_demo_payload()?;
            build_sample_sideload_zip("aweyuki_demo", "AP3A.251105.001", Some(&payload))?
        }
        SampleKind::SuperSparse => build_demo_super_sparse()?,
        SampleKind::FastbootZip => {
            let super_img = build_demo_super_sparse()?;
            let boot_img = make_boot_img(64 * 1024, false);
            let init_boot_img = make_boot_img(48 * 1024, true);
            let vbmeta_img = make_vbmeta_img(0);
            let dtbo_img = make_patterned_fs_image(32 * 1024, 0x22);

            let mut buf = Cursor::new(Vec::new());
            {
                let mut zw = ZipWriter::new(&mut buf);
                let stored =
                    SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
                let deflated =
                    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

                zw.start_file("android-info.txt", deflated)?;
                zw.write_all(b"require board=aweyuki_demo\n")?;

                zw.start_file("fastboot-info.txt", deflated)?;
                zw.write_all(
                    b"version 1\nflash vbmeta vbmeta.img\nflash boot boot.img\nflash init_boot init_boot.img\nflash dtbo dtbo.img\nflash super super.img\n",
                )?;

                zw.start_file("flash_all.sh", deflated)?;
                zw.write_all(
                    b"#!/bin/sh\nfastboot flash vbmeta images/vbmeta.img\nfastboot flash boot images/boot.img\nfastboot flash init_boot images/init_boot.img\nfastboot flash dtbo images/dtbo.img\nfastboot flash super images/super.img\nfastboot oem lock\nfastboot reboot\n",
                )?;

                for (fname, data) in [
                    ("images/vbmeta.img", vbmeta_img),
                    ("images/boot.img", boot_img),
                    ("images/init_boot.img", init_boot_img),
                    ("images/dtbo.img", dtbo_img),
                    ("images/super.img", super_img),
                ] {
                    zw.start_file(fname, stored)?;
                    zw.write_all(&data)?;
                }
                zw.finish()?;
            }
            buf.into_inner()
        }
        SampleKind::RecoverySideloadZip => {
            build_sample_sideload_zip("aweyuki_demo", "LOS-22.2-20261118", None)?
        }
    };
    Ok((name, bytes))
}
