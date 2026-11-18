use crate::error::{FlashError, Result};
use byteorder::{BigEndian, ByteOrder, LittleEndian};
use serde::{Deserialize, Serialize};

pub const ANDROID_BOOT_MAGIC: &[u8; 8] = b"ANDROID!";
pub const VENDOR_BOOT_MAGIC: &[u8; 8] = b"VNDRBOOT";
pub const AVB0_MAGIC: &[u8; 4] = b"AVB0";
pub const AVB_FLAGS_OFFSET: usize = 120;
pub const AVB_FLAG_HASHTREE_DISABLED: u32 = 1 << 0;
pub const AVB_FLAG_VERIFICATION_DISABLED: u32 = 1 << 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BootImageKind {
    Boot,
    InitBoot,
    VendorBoot,
    Recovery,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootImageInfo {
    pub kind: BootImageKind,
    pub header_version: u32,
    pub kernel_size: u32,
    pub ramdisk_size: u32,
    pub page_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VbmetaInfo {
    pub avb_major: u32,
    pub avb_minor: u32,
    pub flags: u32,
    pub hashtree_disabled: bool,
    pub verification_disabled: bool,
}

pub fn inspect_boot_header(header: &[u8]) -> Option<BootImageInfo> {
    if header.len() < 64 {
        return None;
    }
    if &header[0..8] == ANDROID_BOOT_MAGIC {
        let kernel_size = LittleEndian::read_u32(&header[8..12]);
        let ramdisk_size = LittleEndian::read_u32(&header[16..20]);
        let header_version = LittleEndian::read_u32(&header[40..44]);
        let page_size = if header_version >= 3 {
            4096
        } else {
            LittleEndian::read_u32(&header[36..40]).max(2048)
        };
        let kind = if header_version >= 3 && kernel_size == 0 && ramdisk_size > 0 {
            BootImageKind::InitBoot
        } else {
            BootImageKind::Boot
        };
        return Some(BootImageInfo {
            kind,
            header_version,
            kernel_size,
            ramdisk_size,
            page_size,
        });
    }
    if &header[0..8] == VENDOR_BOOT_MAGIC {
        let header_version = LittleEndian::read_u32(&header[8..12]);
        let page_size = LittleEndian::read_u32(&header[12..16]);
        let ramdisk_size = LittleEndian::read_u32(&header[24..28]);
        return Some(BootImageInfo {
            kind: BootImageKind::VendorBoot,
            header_version,
            kernel_size: 0,
            ramdisk_size,
            page_size,
        });
    }
    None
}

pub fn inspect_vbmeta(data: &[u8]) -> Option<VbmetaInfo> {
    if data.len() < 256 || &data[0..4] != AVB0_MAGIC {
        return None;
    }
    let avb_major = BigEndian::read_u32(&data[4..8]);
    let avb_minor = BigEndian::read_u32(&data[8..12]);
    let flags = BigEndian::read_u32(&data[AVB_FLAGS_OFFSET..AVB_FLAGS_OFFSET + 4]);
    Some(VbmetaInfo {
        avb_major,
        avb_minor,
        flags,
        hashtree_disabled: (flags & AVB_FLAG_HASHTREE_DISABLED) != 0,
        verification_disabled: (flags & AVB_FLAG_VERIFICATION_DISABLED) != 0,
    })
}

/// Patch AVB0 header flags in-place at offset 120..124 (big-endian u32).
/// Matches `avbtool make_vbmeta_image --flags 3` / `fastboot --disable-verity --disable-verification`.
pub fn patch_vbmeta_verity_flags(
    data: &mut [u8],
    disable_verity: bool,
    disable_verification: bool,
) -> Result<u32> {
    if data.len() < 256 || &data[0..4] != AVB0_MAGIC {
        return Err(FlashError::InvalidFormat(
            "Not a valid AVB0 vbmeta image (missing AVB0 magic)".into(),
        ));
    }
    let mut flags = BigEndian::read_u32(&data[AVB_FLAGS_OFFSET..AVB_FLAGS_OFFSET + 4]);
    if disable_verity {
        flags |= AVB_FLAG_HASHTREE_DISABLED;
    }
    if disable_verification {
        flags |= AVB_FLAG_VERIFICATION_DISABLED;
    }
    BigEndian::write_u32(&mut data[AVB_FLAGS_OFFSET..AVB_FLAGS_OFFSET + 4], flags);
    Ok(flags)
}
