use crate::error::{FlashError, Result};
use crate::reader::SourceReader;
use crate::sparse::{split_raw_to_sparse_chunks, ImageRandomReader};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Write};

pub const LP_PARTITION_RESERVED_BYTES: u64 = 4096;
pub const LP_METADATA_GEOMETRY_SIZE: u64 = 4096;
pub const LP_METADATA_GEOMETRY_MAGIC: u32 = 0x616C_4467;
pub const LP_METADATA_HEADER_MAGIC: u32 = 0x414C_5030;
pub const LP_SECTOR_SIZE: u64 = 512;

pub const LP_TARGET_TYPE_LINEAR: u32 = 0;
pub const LP_TARGET_TYPE_ZERO: u32 = 1;

pub const LP_PARTITION_ATTR_READONLY: u32 = 1 << 0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LpGeometry {
    pub magic: u32,
    pub struct_size: u32,
    pub metadata_max_size: u32,
    pub metadata_slot_count: u32,
    pub logical_block_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LpExtent {
    pub num_sectors: u64,
    pub target_type: u32,
    pub target_data: u64,
    pub target_source: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LpPartition {
    pub name: String,
    pub attributes: u32,
    pub group_name: String,
    pub size_bytes: u64,
    pub extents: Vec<LpExtent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LpGroup {
    pub name: String,
    pub flags: u32,
    pub maximum_size: u64,
    pub used_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LpBlockDevice {
    pub partition_name: String,
    pub first_logical_sector: u64,
    pub alignment: u32,
    pub alignment_offset: u32,
    pub size: u64,
    pub flags: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuperInfo {
    pub is_sparse: bool,
    pub raw_size: u64,
    pub major_version: u16,
    pub minor_version: u16,
    pub geometry: LpGeometry,
    pub partitions: Vec<LpPartition>,
    pub groups: Vec<LpGroup>,
    pub block_devices: Vec<LpBlockDevice>,
}

#[derive(Debug, Clone)]
struct TableDesc {
    offset: u32,
    num_entries: u32,
    entry_size: u32,
}

fn read_cstr36(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn write_cstr36(dst: &mut [u8], name: &str) {
    dst.fill(0);
    let raw = name.as_bytes();
    let n = raw.len().min(dst.len().saturating_sub(1));
    dst[..n].copy_from_slice(&raw[..n]);
}

fn parse_geometry_at(img: &ImageRandomReader, offset: u64) -> Result<LpGeometry> {
    if offset + 52 > img.raw_len() {
        return Err(FlashError::InvalidFormat(
            "Image too small to contain LpMetadataGeometry".into(),
        ));
    }
    let mut raw = [0u8; 52];
    img.read_exact_at(offset, &mut raw)?;
    let magic = u32::from_le_bytes(raw[0..4].try_into().unwrap());
    if magic != LP_METADATA_GEOMETRY_MAGIC {
        return Err(FlashError::InvalidFormat(format!(
            "Invalid LpMetadataGeometry magic at 0x{offset:x}: 0x{magic:08x}"
        )));
    }
    let struct_size = u32::from_le_bytes(raw[4..8].try_into().unwrap());
    if struct_size < 52 || struct_size > 4096 {
        return Err(FlashError::InvalidFormat(format!(
            "Unexpected LpMetadataGeometry struct_size: {struct_size}"
        )));
    }
    let mut full = vec![0u8; struct_size as usize];
    img.read_exact_at(offset, &mut full)?;
    let expected_sum = full[8..40].to_vec();
    full[8..40].fill(0);
    let actual_sum = Sha256::digest(&full);
    if actual_sum.as_slice() != expected_sum.as_slice() {
        return Err(FlashError::ChecksumMismatch {
            context: "LpMetadataGeometry".into(),
            expected: hex::encode(&expected_sum),
            actual: hex::encode(actual_sum),
        });
    }
    let metadata_max_size = u32::from_le_bytes(full[40..44].try_into().unwrap());
    let metadata_slot_count = u32::from_le_bytes(full[44..48].try_into().unwrap());
    let logical_block_size = u32::from_le_bytes(full[48..52].try_into().unwrap());
    Ok(LpGeometry {
        magic,
        struct_size,
        metadata_max_size,
        metadata_slot_count,
        logical_block_size,
    })
}

pub fn has_super_magic(img: &ImageRandomReader) -> bool {
    parse_geometry_at(img, LP_PARTITION_RESERVED_BYTES).is_ok()
        || parse_geometry_at(img, LP_PARTITION_RESERVED_BYTES + LP_METADATA_GEOMETRY_SIZE).is_ok()
}

pub fn inspect_super(source: SourceReader) -> Result<SuperInfo> {
    let img = ImageRandomReader::open(source)?;
    inspect_super_reader(&img)
}

pub fn inspect_super_reader(img: &ImageRandomReader) -> Result<SuperInfo> {
    let geometry = parse_geometry_at(img, LP_PARTITION_RESERVED_BYTES)
        .or_else(|_| parse_geometry_at(img, LP_PARTITION_RESERVED_BYTES + LP_METADATA_GEOMETRY_SIZE))?;

    let slot0_offset = LP_PARTITION_RESERVED_BYTES + 2 * LP_METADATA_GEOMETRY_SIZE;
    let mut hdr_probe = [0u8; 128];
    img.read_exact_at(slot0_offset, &mut hdr_probe)?;

    let mut cur = Cursor::new(&hdr_probe[..]);
    let magic = cur.read_u32::<LittleEndian>()?;
    if magic != LP_METADATA_HEADER_MAGIC {
        return Err(FlashError::InvalidFormat(format!(
            "Invalid LpMetadataHeader magic: 0x{magic:08x}"
        )));
    }
    let major_version = cur.read_u16::<LittleEndian>()?;
    let minor_version = cur.read_u16::<LittleEndian>()?;
    let header_size = cur.read_u32::<LittleEndian>()?;
    if header_size < 128 || header_size > 4096 {
        return Err(FlashError::InvalidFormat(format!(
            "Unexpected LpMetadataHeader size: {header_size}"
        )));
    }

    let mut full_hdr = vec![0u8; header_size as usize];
    img.read_exact_at(slot0_offset, &mut full_hdr)?;
    let expected_hdr_sum = full_hdr[12..44].to_vec();
    full_hdr[12..44].fill(0);
    let actual_hdr_sum = Sha256::digest(&full_hdr);
    if actual_hdr_sum.as_slice() != expected_hdr_sum.as_slice() {
        return Err(FlashError::ChecksumMismatch {
            context: "LpMetadataHeader".into(),
            expected: hex::encode(&expected_hdr_sum),
            actual: hex::encode(actual_hdr_sum),
        });
    }

    let mut hcur = Cursor::new(&full_hdr[44..]);
    let tables_size = hcur.read_u32::<LittleEndian>()?;
    let mut expected_tables_sum = [0u8; 32];
    std::io::Read::read_exact(&mut hcur, &mut expected_tables_sum)?;

    let read_desc = |c: &mut Cursor<&[u8]>| -> Result<TableDesc> {
        Ok(TableDesc {
            offset: c.read_u32::<LittleEndian>()?,
            num_entries: c.read_u32::<LittleEndian>()?,
            entry_size: c.read_u32::<LittleEndian>()?,
        })
    };
    let part_desc = read_desc(&mut hcur)?;
    let ext_desc = read_desc(&mut hcur)?;
    let grp_desc = read_desc(&mut hcur)?;
    let bdev_desc = read_desc(&mut hcur)?;

    if tables_size > geometry.metadata_max_size {
        return Err(FlashError::SizeLimitExceeded(
            "Metadata tables_size exceeds metadata_max_size".into(),
        ));
    }

    let mut tables = vec![0u8; tables_size as usize];
    img.read_exact_at(slot0_offset + header_size as u64, &mut tables)?;
    let actual_tables_sum = Sha256::digest(&tables);
    if actual_tables_sum.as_slice() != expected_tables_sum {
        return Err(FlashError::ChecksumMismatch {
            context: "LpMetadata tables".into(),
            expected: hex::encode(expected_tables_sum),
            actual: hex::encode(actual_tables_sum),
        });
    }

    // Parse extents
    let mut all_extents = Vec::with_capacity(ext_desc.num_entries as usize);
    for i in 0..ext_desc.num_entries {
        let base = (ext_desc.offset + i * ext_desc.entry_size) as usize;
        if base + 24 > tables.len() {
            return Err(FlashError::InvalidFormat("Extent table out of bounds".into()));
        }
        let mut c = Cursor::new(&tables[base..base + 24]);
        all_extents.push(LpExtent {
            num_sectors: c.read_u64::<LittleEndian>()?,
            target_type: c.read_u32::<LittleEndian>()?,
            target_data: c.read_u64::<LittleEndian>()?,
            target_source: c.read_u32::<LittleEndian>()?,
        });
    }

    // Parse groups
    let mut groups = Vec::with_capacity(grp_desc.num_entries as usize);
    for i in 0..grp_desc.num_entries {
        let base = (grp_desc.offset + i * grp_desc.entry_size) as usize;
        if base + 48 > tables.len() {
            return Err(FlashError::InvalidFormat("Group table out of bounds".into()));
        }
        let name = read_cstr36(&tables[base..base + 36]);
        let mut c = Cursor::new(&tables[base + 36..base + 48]);
        let flags = c.read_u32::<LittleEndian>()?;
        let maximum_size = c.read_u64::<LittleEndian>()?;
        groups.push(LpGroup {
            name,
            flags,
            maximum_size,
            used_bytes: 0,
        });
    }

    // Parse block devices
    let mut block_devices = Vec::with_capacity(bdev_desc.num_entries as usize);
    for i in 0..bdev_desc.num_entries {
        let base = (bdev_desc.offset + i * bdev_desc.entry_size) as usize;
        if base + 64 > tables.len() {
            return Err(FlashError::InvalidFormat("Block device table out of bounds".into()));
        }
        let mut c = Cursor::new(&tables[base..base + 24]);
        let first_logical_sector = c.read_u64::<LittleEndian>()?;
        let alignment = c.read_u32::<LittleEndian>()?;
        let alignment_offset = c.read_u32::<LittleEndian>()?;
        let size = c.read_u64::<LittleEndian>()?;
        let partition_name = read_cstr36(&tables[base + 24..base + 60]);
        let flags = u32::from_le_bytes(tables[base + 60..base + 64].try_into().unwrap());
        block_devices.push(LpBlockDevice {
            partition_name,
            first_logical_sector,
            alignment,
            alignment_offset,
            size,
            flags,
        });
    }

    // Parse partitions
    let mut partitions = Vec::with_capacity(part_desc.num_entries as usize);
    for i in 0..part_desc.num_entries {
        let base = (part_desc.offset + i * part_desc.entry_size) as usize;
        if base + 52 > tables.len() {
            return Err(FlashError::InvalidFormat("Partition table out of bounds".into()));
        }
        let name = read_cstr36(&tables[base..base + 36]);
        let mut c = Cursor::new(&tables[base + 36..base + 52]);
        let attributes = c.read_u32::<LittleEndian>()?;
        let first_extent_index = c.read_u32::<LittleEndian>()? as usize;
        let num_extents = c.read_u32::<LittleEndian>()? as usize;
        let group_index = c.read_u32::<LittleEndian>()? as usize;

        let end_idx = first_extent_index.saturating_add(num_extents);
        if end_idx > all_extents.len() {
            return Err(FlashError::InvalidFormat(
                "Partition extent index exceeds extent table".into(),
            ));
        }
        let extents = all_extents[first_extent_index..end_idx].to_vec();
        let size_bytes: u64 = extents.iter().map(|e| e.num_sectors * LP_SECTOR_SIZE).sum();
        let group_name = groups
            .get(group_index)
            .map(|g| g.name.clone())
            .unwrap_or_else(|| "default".to_string());
        if let Some(grp) = groups.get_mut(group_index) {
            grp.used_bytes += size_bytes;
        }

        partitions.push(LpPartition {
            name,
            attributes,
            group_name,
            size_bytes,
            extents,
        });
    }

    Ok(SuperInfo {
        is_sparse: img.is_sparse(),
        raw_size: img.raw_len(),
        major_version,
        minor_version,
        geometry,
        partitions,
        groups,
        block_devices,
    })
}

pub fn unpack_super_partition<W: Write>(
    source: SourceReader,
    partition_name: &str,
    out: &mut W,
) -> Result<u64> {
    let img = ImageRandomReader::open(source)?;
    let info = inspect_super_reader(&img)?;
    let part = info
        .partitions
        .iter()
        .find(|p| p.name == partition_name)
        .ok_or_else(|| FlashError::PartitionNotFound(partition_name.to_string()))?;

    let mut written: u64 = 0;
    let mut buf = vec![0u8; 256 * 1024];
    for ext in &part.extents {
        let mut rem = ext.num_sectors * LP_SECTOR_SIZE;
        match ext.target_type {
            LP_TARGET_TYPE_LINEAR => {
                let mut off = ext.target_data * LP_SECTOR_SIZE;
                while rem > 0 {
                    let take = rem.min(buf.len() as u64) as usize;
                    img.read_exact_at(off, &mut buf[..take])?;
                    out.write_all(&buf[..take])?;
                    off += take as u64;
                    rem -= take as u64;
                    written += take as u64;
                }
            }
            LP_TARGET_TYPE_ZERO => {
                buf.fill(0);
                while rem > 0 {
                    let take = rem.min(buf.len() as u64) as usize;
                    out.write_all(&buf[..take])?;
                    rem -= take as u64;
                    written += take as u64;
                }
            }
            other => {
                return Err(FlashError::Unsupported(format!(
                    "Unsupported LP extent target type {other}"
                )));
            }
        }
    }
    out.flush()?;
    Ok(written)
}

pub fn unpack_super_partition_bytes(source: SourceReader, partition_name: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    unpack_super_partition(source, partition_name, &mut out)?;
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct SuperInputPartition {
    pub name: String,
    pub group: String,
    pub data: Vec<u8>,
    pub readonly: bool,
}

/// Build an AOSP-compliant `super.img` (readable by `/usr/bin/lpdump` & `/usr/bin/lpunpack`),
/// optionally wrapped as an Android Sparse image.
pub fn build_super_image(
    device_size: u64,
    metadata_max_size: u32,
    partitions: &[SuperInputPartition],
    as_sparse: bool,
) -> Result<Vec<u8>> {
    let slot_count: u32 = 2;
    let logical_block_size: u32 = 4096;
    let first_logical_sector: u64 = 2048; // 1 MiB offset, strictly >= 0x3000 + 4 * metadata_max_size when metadata_max_size <= 65536
    let min_first_bytes =
        LP_PARTITION_RESERVED_BYTES + 2 * LP_METADATA_GEOMETRY_SIZE + 4 * metadata_max_size as u64;
    let data_start_sector = first_logical_sector.max(min_first_bytes.div_ceil(LP_SECTOR_SIZE));
    let align_sectors = (logical_block_size as u64) / LP_SECTOR_SIZE;
    let align_up_sector = |s: u64| -> u64 { s.div_ceil(align_sectors) * align_sectors };
    let data_start_sector = align_up_sector(data_start_sector);

    // Build groups ("default" first, then unique group names)
    let mut group_names = vec!["default".to_string()];
    for p in partitions {
        if !group_names.contains(&p.group) {
            group_names.push(p.group.clone());
        }
    }

    let mut cur_sector = data_start_sector;
    let mut part_records = Vec::<(String, u32, u32, u32, u32)>::new();
    let mut extent_records = Vec::<(u64, u32, u64, u32)>::new();
    let mut placements = Vec::<(u64, Vec<u8>)>::new();
    let mut group_used = std::collections::HashMap::<String, u64>::new();

    for p in partitions {
        let aligned_len = p.data.len().max(logical_block_size as usize).div_ceil(logical_block_size as usize)
            * logical_block_size as usize;
        let num_sectors = (aligned_len as u64) / LP_SECTOR_SIZE;
        let start_byte = cur_sector * LP_SECTOR_SIZE;
        let mut padded = vec![0u8; aligned_len];
        padded[..p.data.len()].copy_from_slice(&p.data);

        let first_ext = extent_records.len() as u32;
        extent_records.push((num_sectors, LP_TARGET_TYPE_LINEAR, cur_sector, 0));
        let grp_idx = group_names.iter().position(|g| g == &p.group).unwrap_or(0) as u32;
        let attrs = if p.readonly { LP_PARTITION_ATTR_READONLY } else { 0 };
        part_records.push((p.name.clone(), attrs, first_ext, 1, grp_idx));
        *group_used.entry(p.group.clone()).or_insert(0) += aligned_len as u64;
        placements.push((start_byte, padded));
        cur_sector += num_sectors;
    }

    let min_device_size = cur_sector * LP_SECTOR_SIZE + 1024 * 1024;
    let total_device_size = device_size.max(min_device_size);

    // Serialize tables
    let mut tables = Vec::new();
    let parts_offset = tables.len() as u32;
    for (name, attrs, first_ext, num_ext, grp_idx) in &part_records {
        let mut entry = vec![0u8; 52];
        write_cstr36(&mut entry[0..36], name);
        (&mut entry[36..40]).write_u32::<LittleEndian>(*attrs)?;
        (&mut entry[40..44]).write_u32::<LittleEndian>(*first_ext)?;
        (&mut entry[44..48]).write_u32::<LittleEndian>(*num_ext)?;
        (&mut entry[48..52]).write_u32::<LittleEndian>(*grp_idx)?;
        tables.extend_from_slice(&entry);
    }

    let extents_offset = tables.len() as u32;
    for (num_sectors, ttype, tdata, tsrc) in &extent_records {
        tables.write_u64::<LittleEndian>(*num_sectors)?;
        tables.write_u32::<LittleEndian>(*ttype)?;
        tables.write_u64::<LittleEndian>(*tdata)?;
        tables.write_u32::<LittleEndian>(*tsrc)?;
    }

    let groups_offset = tables.len() as u32;
    for gname in &group_names {
        let mut entry = vec![0u8; 48];
        write_cstr36(&mut entry[0..36], gname);
        let max_sz = if gname == "default" {
            0
        } else {
            total_device_size - data_start_sector * LP_SECTOR_SIZE
        };
        (&mut entry[36..40]).write_u32::<LittleEndian>(0)?;
        (&mut entry[40..48]).write_u64::<LittleEndian>(max_sz)?;
        tables.extend_from_slice(&entry);
    }

    let bdev_offset = tables.len() as u32;
    let mut bdev = vec![0u8; 64];
    (&mut bdev[0..8]).write_u64::<LittleEndian>(data_start_sector)?;
    (&mut bdev[8..12]).write_u32::<LittleEndian>(1024 * 1024)?; // 1 MiB alignment
    (&mut bdev[12..16]).write_u32::<LittleEndian>(0)?;
    (&mut bdev[16..24]).write_u64::<LittleEndian>(total_device_size)?;
    write_cstr36(&mut bdev[24..60], "super");
    (&mut bdev[60..64]).write_u32::<LittleEndian>(0)?;
    tables.extend_from_slice(&bdev);

    let tables_checksum = Sha256::digest(&tables);
    let header_size = 128u32;
    let mut hdr = vec![0u8; header_size as usize];
    (&mut hdr[0..4]).write_u32::<LittleEndian>(LP_METADATA_HEADER_MAGIC)?;
    (&mut hdr[4..6]).write_u16::<LittleEndian>(10)?;
    (&mut hdr[6..8]).write_u16::<LittleEndian>(0)?;
    (&mut hdr[8..12]).write_u32::<LittleEndian>(header_size)?;
    (&mut hdr[44..48]).write_u32::<LittleEndian>(tables.len() as u32)?;
    hdr[48..80].copy_from_slice(&tables_checksum);
    let mut desc_cur = Cursor::new(&mut hdr[80..128]);
    for (off, num, esz) in [
        (parts_offset, part_records.len() as u32, 52u32),
        (extents_offset, extent_records.len() as u32, 24u32),
        (groups_offset, group_names.len() as u32, 48u32),
        (bdev_offset, 1u32, 64u32),
    ] {
        desc_cur.write_u32::<LittleEndian>(off)?;
        desc_cur.write_u32::<LittleEndian>(num)?;
        desc_cur.write_u32::<LittleEndian>(esz)?;
    }
    let hdr_checksum = Sha256::digest(&hdr);
    hdr[12..44].copy_from_slice(&hdr_checksum);

    let mut geom = vec![0u8; 52];
    (&mut geom[0..4]).write_u32::<LittleEndian>(LP_METADATA_GEOMETRY_MAGIC)?;
    (&mut geom[4..8]).write_u32::<LittleEndian>(52)?;
    (&mut geom[40..44]).write_u32::<LittleEndian>(metadata_max_size)?;
    (&mut geom[44..48]).write_u32::<LittleEndian>(slot_count)?;
    (&mut geom[48..52]).write_u32::<LittleEndian>(logical_block_size)?;
    let geom_checksum = Sha256::digest(&geom);
    geom[8..40].copy_from_slice(&geom_checksum);

    let mut raw = vec![0u8; total_device_size as usize];
    raw[0x1000..0x1000 + 52].copy_from_slice(&geom);
    raw[0x2000..0x2000 + 52].copy_from_slice(&geom);

    // Primary slots 0..slot_count + backup slots
    for copy_idx in 0..(slot_count * 2) {
        let base = 0x3000 + (copy_idx as usize) * (metadata_max_size as usize);
        if base + hdr.len() + tables.len() <= raw.len() {
            raw[base..base + hdr.len()].copy_from_slice(&hdr);
            raw[base + hdr.len()..base + hdr.len() + tables.len()].copy_from_slice(&tables);
        }
    }

    for (start_byte, bytes) in placements {
        let off = start_byte as usize;
        raw[off..off + bytes.len()].copy_from_slice(&bytes);
    }

    if as_sparse {
        let mut cur = Cursor::new(&raw);
        let chunks = split_raw_to_sparse_chunks(&mut cur, raw.len() as u64, 4096, u64::MAX / 2)?;
        Ok(chunks.into_iter().next().map(|c| c.data).unwrap_or_default())
    } else {
        Ok(raw)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogicalLayoutAction {
    DeletePartition(String),
    CreatePartition { name: String, size_bytes: u64 },
    ResizePartition { name: String, size_bytes: u64 },
}

pub struct SuperLayoutManager;

impl SuperLayoutManager {
    /// Generate fastbootd logical partition management operations before flashing unpacked
    /// dynamic partitions (reclaiming `-cow` snapshots and stale partitions cleanly).
    pub fn plan_resize_actions(
        existing_device_logicals: &[String],
        target_partitions: &[(String, u64)],
        reclaim_cow: bool,
    ) -> Vec<LogicalLayoutAction> {
        let mut actions = Vec::new();
        if reclaim_cow {
            for name in existing_device_logicals {
                if name.ends_with("-cow") {
                    actions.push(LogicalLayoutAction::DeletePartition(name.clone()));
                }
            }
        }
        for (name, size_bytes) in target_partitions {
            if existing_device_logicals.contains(name) {
                actions.push(LogicalLayoutAction::ResizePartition {
                    name: name.clone(),
                    size_bytes: *size_bytes,
                });
            } else {
                actions.push(LogicalLayoutAction::CreatePartition {
                    name: name.clone(),
                    size_bytes: *size_bytes,
                });
            }
        }
        actions
    }
}
