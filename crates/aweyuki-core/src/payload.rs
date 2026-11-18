use crate::error::{FlashError, Result};
use crate::reader::SourceReader;
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use prost::Message;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};

pub const PAYLOAD_MAGIC: &[u8; 4] = b"CrAU";
pub const PAYLOAD_MAJOR_VERSION: u64 = 2;

pub const OP_REPLACE: i32 = 0;
pub const OP_REPLACE_BZ: i32 = 1;
pub const OP_MOVE: i32 = 2;
pub const OP_BSDIFF: i32 = 3;
pub const OP_SOURCE_COPY: i32 = 4;
pub const OP_SOURCE_BSDIFF: i32 = 5;
pub const OP_ZERO: i32 = 6;
pub const OP_DISCARD: i32 = 7;
pub const OP_REPLACE_XZ: i32 = 8;
pub const OP_PUFFDIFF: i32 = 9;
pub const OP_BROTLI_BSDIFF: i32 = 10;
pub const OP_ZUCCHINI: i32 = 11;
pub const OP_LZ4DIFF_BSDIFF: i32 = 12;
pub const OP_LZ4DIFF_PUFFDIFF: i32 = 13;
pub const OP_ZSTD: i32 = 14;

pub mod proto {
    use prost::Message;

    #[derive(Clone, PartialEq, Message)]
    pub struct Extent {
        #[prost(uint64, optional, tag = "1")]
        pub start_block: Option<u64>,
        #[prost(uint64, optional, tag = "2")]
        pub num_blocks: Option<u64>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct PartitionInfo {
        #[prost(uint64, optional, tag = "1")]
        pub size: Option<u64>,
        #[prost(bytes = "vec", optional, tag = "2")]
        pub hash: Option<Vec<u8>>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct InstallOperation {
        #[prost(int32, required, tag = "1")]
        pub r#type: i32,
        #[prost(uint64, optional, tag = "2")]
        pub data_offset: Option<u64>,
        #[prost(uint64, optional, tag = "3")]
        pub data_length: Option<u64>,
        #[prost(message, repeated, tag = "4")]
        pub src_extents: Vec<Extent>,
        #[prost(uint64, optional, tag = "5")]
        pub src_length: Option<u64>,
        #[prost(message, repeated, tag = "6")]
        pub dst_extents: Vec<Extent>,
        #[prost(uint64, optional, tag = "7")]
        pub dst_length: Option<u64>,
        #[prost(bytes = "vec", optional, tag = "8")]
        pub data_sha256_hash: Option<Vec<u8>>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct PartitionUpdate {
        #[prost(string, required, tag = "1")]
        pub partition_name: String,
        #[prost(bool, optional, tag = "2")]
        pub run_postinstall: Option<bool>,
        #[prost(message, optional, tag = "6")]
        pub old_partition_info: Option<PartitionInfo>,
        #[prost(message, optional, tag = "7")]
        pub new_partition_info: Option<PartitionInfo>,
        #[prost(message, repeated, tag = "8")]
        pub operations: Vec<InstallOperation>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct DynamicPartitionGroup {
        #[prost(string, required, tag = "1")]
        pub name: String,
        #[prost(uint64, optional, tag = "2")]
        pub size: Option<u64>,
        #[prost(string, repeated, tag = "3")]
        pub partition_names: Vec<String>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct DynamicPartitionMetadata {
        #[prost(message, repeated, tag = "1")]
        pub groups: Vec<DynamicPartitionGroup>,
        #[prost(bool, optional, tag = "2")]
        pub snapshot_enabled: Option<bool>,
        #[prost(bool, optional, tag = "3")]
        pub vabc_enabled: Option<bool>,
    }

    #[derive(Clone, PartialEq, Message)]
    pub struct DeltaArchiveManifest {
        #[prost(uint32, optional, tag = "3")]
        pub block_size: Option<u32>,
        #[prost(uint32, optional, tag = "12")]
        pub minor_version: Option<u32>,
        #[prost(message, repeated, tag = "13")]
        pub partitions: Vec<PartitionUpdate>,
        #[prost(int64, optional, tag = "14")]
        pub max_timestamp: Option<i64>,
        #[prost(message, optional, tag = "15")]
        pub dynamic_partition_metadata: Option<DynamicPartitionMetadata>,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayloadDynamicGroupInfo {
    pub name: String,
    pub max_size: u64,
    pub partition_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayloadPartitionSummary {
    pub name: String,
    pub size: u64,
    pub sha256_hex: Option<String>,
    pub op_count: usize,
    pub op_summary: String,
    pub is_dynamic: bool,
    pub is_incremental: bool,
}

#[derive(Debug, Clone)]
pub struct ParsedPayload {
    pub version: u64,
    pub block_size: u32,
    pub minor_version: u32,
    pub is_incremental: bool,
    pub blob_offset_base: u64,
    pub max_timestamp: Option<i64>,
    pub snapshot_enabled: bool,
    pub dynamic_groups: Vec<PayloadDynamicGroupInfo>,
    pub manifest: proto::DeltaArchiveManifest,
    pub partitions: Vec<PayloadPartitionSummary>,
}

pub fn is_payload_magic(prefix: &[u8]) -> bool {
    prefix.len() >= 4 && &prefix[0..4] == PAYLOAD_MAGIC
}

pub fn op_type_name(op_type: i32) -> &'static str {
    match op_type {
        OP_REPLACE => "REPLACE",
        OP_REPLACE_BZ => "REPLACE_BZ",
        OP_MOVE => "MOVE",
        OP_BSDIFF => "BSDIFF",
        OP_SOURCE_COPY => "SOURCE_COPY",
        OP_SOURCE_BSDIFF => "SOURCE_BSDIFF",
        OP_ZERO => "ZERO",
        OP_DISCARD => "DISCARD",
        OP_REPLACE_XZ => "REPLACE_XZ",
        OP_PUFFDIFF => "PUFFDIFF",
        OP_BROTLI_BSDIFF => "BROTLI_BSDIFF",
        OP_ZUCCHINI => "ZUCCHINI",
        OP_LZ4DIFF_BSDIFF => "LZ4DIFF_BSDIFF",
        OP_LZ4DIFF_PUFFDIFF => "LZ4DIFF_PUFFDIFF",
        OP_ZSTD => "ZSTD",
        _ => "UNKNOWN",
    }
}

fn is_delta_op(op_type: i32) -> bool {
    matches!(
        op_type,
        OP_MOVE
            | OP_BSDIFF
            | OP_SOURCE_COPY
            | OP_SOURCE_BSDIFF
            | OP_PUFFDIFF
            | OP_BROTLI_BSDIFF
            | OP_ZUCCHINI
            | OP_LZ4DIFF_BSDIFF
            | OP_LZ4DIFF_PUFFDIFF
    )
}

pub fn parse_payload(reader: &mut SourceReader) -> Result<ParsedPayload> {
    reader.seek(SeekFrom::Start(0))?;
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;
    if &magic != PAYLOAD_MAGIC {
        return Err(FlashError::InvalidFormat(
            "Invalid payload.bin header (missing CrAU magic)".into(),
        ));
    }

    let version = reader.read_u64::<BigEndian>()?;
    if version != PAYLOAD_MAJOR_VERSION {
        return Err(FlashError::Unsupported(format!(
            "Unsupported payload major version {version} (expected 2)"
        )));
    }
    let manifest_size = reader.read_u64::<BigEndian>()?;
    let metadata_sig_size = reader.read_u32::<BigEndian>()?;
    if manifest_size > 256 * 1024 * 1024 {
        return Err(FlashError::SizeLimitExceeded(
            "Manifest size exceeds 256 MiB sanity limit".into(),
        ));
    }

    let mut manifest_bytes = vec![0u8; manifest_size as usize];
    reader.read_exact(&mut manifest_bytes)?;
    let manifest = proto::DeltaArchiveManifest::decode(&manifest_bytes[..])?;

    let block_size = manifest.block_size.unwrap_or(4096);
    let minor_version = manifest.minor_version.unwrap_or(0);
    let blob_offset_base = 24u64 + manifest_size + metadata_sig_size as u64;

    let mut dynamic_groups = Vec::new();
    let mut dynamic_names = std::collections::HashSet::new();
    let mut snapshot_enabled = false;

    if let Some(dpm) = &manifest.dynamic_partition_metadata {
        snapshot_enabled = dpm.snapshot_enabled.unwrap_or(false) || dpm.vabc_enabled.unwrap_or(false);
        for g in &dpm.groups {
            for p in &g.partition_names {
                dynamic_names.insert(p.clone());
            }
            dynamic_groups.push(PayloadDynamicGroupInfo {
                name: g.name.clone(),
                max_size: g.size.unwrap_or(0),
                partition_names: g.partition_names.clone(),
            });
        }
    }

    let mut any_incremental = minor_version > 0;
    let mut partitions = Vec::with_capacity(manifest.partitions.len());
    for part in &manifest.partitions {
        let size = part
            .new_partition_info
            .as_ref()
            .and_then(|info| info.size)
            .unwrap_or_else(|| {
                part.operations
                    .iter()
                    .flat_map(|op| op.dst_extents.iter())
                    .map(|ext| {
                        (ext.start_block.unwrap_or(0) + ext.num_blocks.unwrap_or(0))
                            * block_size as u64
                    })
                    .max()
                    .unwrap_or(0)
            });

        let sha256_hex = part
            .new_partition_info
            .as_ref()
            .and_then(|info| info.hash.as_ref())
            .map(hex::encode);

        let part_incremental =
            part.old_partition_info.is_some() || part.operations.iter().any(|op| is_delta_op(op.r#type));
        if part_incremental {
            any_incremental = true;
        }

        let mut counts = std::collections::BTreeMap::<&'static str, usize>::new();
        for op in &part.operations {
            *counts.entry(op_type_name(op.r#type)).or_insert(0) += 1;
        }
        let op_summary = counts
            .into_iter()
            .map(|(k, v)| format!("{k}:{v}"))
            .collect::<Vec<_>>()
            .join(", ");

        partitions.push(PayloadPartitionSummary {
            name: part.partition_name.clone(),
            size,
            sha256_hex,
            op_count: part.operations.len(),
            op_summary,
            is_dynamic: dynamic_names.contains(&part.partition_name),
            is_incremental: part_incremental,
        });
    }

    Ok(ParsedPayload {
        version,
        block_size,
        minor_version,
        is_incremental: any_incremental,
        blob_offset_base,
        max_timestamp: manifest.max_timestamp,
        snapshot_enabled,
        dynamic_groups,
        manifest,
        partitions,
    })
}

fn write_across_extents<W: Write + Seek>(
    out: &mut W,
    block_size: u64,
    extents: &[proto::Extent],
    mut data: &[u8],
) -> Result<()> {
    for ext in extents {
        let start_byte = ext.start_block.unwrap_or(0) * block_size;
        let ext_bytes = (ext.num_blocks.unwrap_or(0) * block_size) as usize;
        out.seek(SeekFrom::Start(start_byte))?;
        let take = data.len().min(ext_bytes);
        if take > 0 {
            out.write_all(&data[..take])?;
            data = &data[take..];
        }
        if take < ext_bytes {
            let pad = vec![0u8; ext_bytes - take];
            out.write_all(&pad)?;
        }
    }
    Ok(())
}

/// Extract a single partition from `payload.bin` into a random-access Read+Write+Seek target
/// (either `Cursor<Vec<u8>>` or `std::fs::File`), verifying both per-operation blob hashes
/// and the final `new_partition_info` SHA-256 hash.
pub fn extract_payload_partition<RW: Read + Write + Seek>(
    reader: &SourceReader,
    parsed: &ParsedPayload,
    partition_name: &str,
    out: &mut RW,
    mut on_progress: impl FnMut(usize, usize),
) -> Result<u64> {
    let part = parsed
        .manifest
        .partitions
        .iter()
        .find(|p| p.partition_name == partition_name)
        .ok_or_else(|| FlashError::PartitionNotFound(partition_name.to_string()))?;

    let summary = parsed
        .partitions
        .iter()
        .find(|p| p.name == partition_name)
        .ok_or_else(|| FlashError::PartitionNotFound(partition_name.to_string()))?;

    let block_size = parsed.block_size as u64;
    let total_ops = part.operations.len();

    // Ensure target output reaches full partition size even if trailing extents are omitted
    if summary.size > 0 {
        out.seek(SeekFrom::Start(summary.size - 1))?;
        out.write_all(&[0])?;
        out.seek(SeekFrom::Start(0))?;
    }

    for (idx, op) in part.operations.iter().enumerate() {
        on_progress(idx + 1, total_ops);
        if is_delta_op(op.r#type) {
            return Err(FlashError::Unsupported(format!(
                "Partition '{partition_name}' contains incremental delta operation ({}). Incremental OTAs require adb sideload or an existing base partition.",
                op_type_name(op.r#type)
            )));
        }

        let dst_total_bytes: usize = op
            .dst_extents
            .iter()
            .map(|e| (e.num_blocks.unwrap_or(0) * block_size) as usize)
            .sum();

        match op.r#type {
            OP_ZERO | OP_DISCARD => {
                let zeros = vec![0u8; dst_total_bytes];
                write_across_extents(out, block_size, &op.dst_extents, &zeros)?;
            }
            OP_REPLACE | OP_REPLACE_XZ | OP_REPLACE_BZ | OP_ZSTD => {
                let rel_off = op.data_offset.ok_or_else(|| {
                    FlashError::InvalidFormat("Missing data_offset in install op".into())
                })?;
                let data_len = op.data_length.ok_or_else(|| {
                    FlashError::InvalidFormat("Missing data_length in install op".into())
                })? as usize;

                let blob = reader.read_vec_at(parsed.blob_offset_base + rel_off, data_len)?;

                if let Some(expected_hash) = &op.data_sha256_hash {
                    if !expected_hash.is_empty() {
                        let actual = Sha256::digest(&blob);
                        if actual.as_slice() != expected_hash.as_slice() {
                            return Err(FlashError::ChecksumMismatch {
                                context: format!("{partition_name} op #{idx}"),
                                expected: hex::encode(expected_hash),
                                actual: hex::encode(actual),
                            });
                        }
                    }
                }

                let decoded = match op.r#type {
                    OP_REPLACE => blob,
                    OP_REPLACE_XZ => {
                        let mut decompressed = Vec::with_capacity(dst_total_bytes);
                        let mut cur = Cursor::new(&blob);
                        lzma_rs::xz_decompress(&mut cur, &mut decompressed).map_err(|e| {
                            FlashError::InvalidFormat(format!("XZ decompress failed: {e}"))
                        })?;
                        decompressed
                    }
                    OP_REPLACE_BZ => {
                        let mut decompressed = Vec::with_capacity(dst_total_bytes);
                        let mut dec = bzip2_rs::DecoderReader::new(Cursor::new(&blob));
                        dec.read_to_end(&mut decompressed)?;
                        decompressed
                    }
                    OP_ZSTD => {
                        let mut decompressed = Vec::with_capacity(dst_total_bytes);
                        let mut dec =
                            ruzstd::StreamingDecoder::new(Cursor::new(&blob)).map_err(|e| {
                                FlashError::InvalidFormat(format!("ZSTD init failed: {e}"))
                            })?;
                        dec.read_to_end(&mut decompressed)?;
                        decompressed
                    }
                    _ => unreachable!(),
                };

                write_across_extents(out, block_size, &op.dst_extents, &decoded)?;
            }
            other => {
                return Err(FlashError::Unsupported(format!(
                    "Unsupported payload operation type: {other}"
                )));
            }
        }
    }

    out.flush()?;

    // Verify full partition SHA-256 against new_partition_info
    if let Some(expected_hex) = &summary.sha256_hex {
        if !expected_hex.is_empty() {
            out.seek(SeekFrom::Start(0))?;
            let mut hasher = Sha256::new();
            let mut buf = vec![0u8; 64 * 1024];
            let mut rem = summary.size;
            while rem > 0 {
                let take = rem.min(buf.len() as u64) as usize;
                out.read_exact(&mut buf[..take])?;
                hasher.update(&buf[..take]);
                rem -= take as u64;
            }
            let actual_hex = hex::encode(hasher.finalize());
            if &actual_hex != expected_hex {
                return Err(FlashError::ChecksumMismatch {
                    context: format!("partition {partition_name}"),
                    expected: expected_hex.clone(),
                    actual: actual_hex,
                });
            }
        }
    }

    Ok(summary.size)
}

pub fn extract_payload_partition_bytes(
    reader: &SourceReader,
    parsed: &ParsedPayload,
    partition_name: &str,
) -> Result<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::new());
    extract_payload_partition(reader, parsed, partition_name, &mut cursor, |_, _| {})?;
    Ok(cursor.into_inner())
}

#[derive(Debug, Clone)]
pub struct PayloadBuildPartition {
    pub name: String,
    pub data: Vec<u8>,
    pub compress_xz: bool,
    pub dynamic_group: Option<String>,
}

/// Encode a valid AOSP CrAU v2 `payload.bin` (used for testing and Web/CLI sample ROMs).
pub fn build_payload_bytes(
    partitions: &[PayloadBuildPartition],
    block_size: u32,
) -> Result<Vec<u8>> {
    let mut blobs = Vec::<u8>::new();
    let mut proto_parts = Vec::<proto::PartitionUpdate>::new();
    let mut groups_map = std::collections::BTreeMap::<String, (u64, Vec<String>)>::new();

    for part in partitions {
        let blk = block_size as usize;
        let padded_len = if part.data.is_empty() {
            blk
        } else {
            part.data.len().div_ceil(blk) * blk
        };
        let mut padded = vec![0u8; padded_len];
        padded[..part.data.len()].copy_from_slice(&part.data);

        let part_hash = Sha256::digest(&padded).to_vec();
        let total_blks = (padded.len() / blk) as u64;

        let mut ops = Vec::new();
        let all_zero = padded.iter().all(|&b| b == 0);
        if all_zero {
            ops.push(proto::InstallOperation {
                r#type: OP_ZERO,
                data_offset: None,
                data_length: None,
                src_extents: vec![],
                src_length: None,
                dst_extents: vec![proto::Extent {
                    start_block: Some(0),
                    num_blocks: Some(total_blks),
                }],
                dst_length: Some(padded.len() as u64),
                data_sha256_hash: None,
            });
        } else if part.compress_xz {
            let mut compressed = Vec::new();
            let mut cur = Cursor::new(&padded);
            lzma_rs::xz_compress(&mut cur, &mut compressed)
                .map_err(|e| FlashError::InvalidFormat(format!("XZ compress error: {e}")))?;
            let op_hash = Sha256::digest(&compressed).to_vec();
            let offset = blobs.len() as u64;
            let len = compressed.len() as u64;
            blobs.extend_from_slice(&compressed);
            ops.push(proto::InstallOperation {
                r#type: OP_REPLACE_XZ,
                data_offset: Some(offset),
                data_length: Some(len),
                src_extents: vec![],
                src_length: None,
                dst_extents: vec![proto::Extent {
                    start_block: Some(0),
                    num_blocks: Some(total_blks),
                }],
                dst_length: Some(padded.len() as u64),
                data_sha256_hash: Some(op_hash),
            });
        } else {
            let op_hash = Sha256::digest(&padded).to_vec();
            let offset = blobs.len() as u64;
            let len = padded.len() as u64;
            blobs.extend_from_slice(&padded);
            ops.push(proto::InstallOperation {
                r#type: OP_REPLACE,
                data_offset: Some(offset),
                data_length: Some(len),
                src_extents: vec![],
                src_length: None,
                dst_extents: vec![proto::Extent {
                    start_block: Some(0),
                    num_blocks: Some(total_blks),
                }],
                dst_length: Some(padded.len() as u64),
                data_sha256_hash: Some(op_hash),
            });
        }

        if let Some(group) = &part.dynamic_group {
            let entry = groups_map
                .entry(group.clone())
                .or_insert_with(|| (0, Vec::new()));
            entry.0 += padded.len() as u64;
            entry.1.push(part.name.clone());
        }

        proto_parts.push(proto::PartitionUpdate {
            partition_name: part.name.clone(),
            run_postinstall: Some(false),
            old_partition_info: None,
            new_partition_info: Some(proto::PartitionInfo {
                size: Some(padded.len() as u64),
                hash: Some(part_hash),
            }),
            operations: ops,
        });
    }

    let dpm = if groups_map.is_empty() {
        None
    } else {
        Some(proto::DynamicPartitionMetadata {
            groups: groups_map
                .into_iter()
                .map(|(name, (sum_sz, partition_names))| proto::DynamicPartitionGroup {
                    name,
                    size: Some(sum_sz + 64 * 1024 * 1024),
                    partition_names,
                })
                .collect(),
            snapshot_enabled: Some(true),
            vabc_enabled: Some(true),
        })
    };

    let manifest = proto::DeltaArchiveManifest {
        block_size: Some(block_size),
        minor_version: Some(0),
        partitions: proto_parts,
        max_timestamp: Some(1_760_000_000),
        dynamic_partition_metadata: dpm,
    };

    let manifest_bytes = manifest.encode_to_vec();
    let mut out = Vec::with_capacity(24 + manifest_bytes.len() + blobs.len());
    out.write_all(PAYLOAD_MAGIC)?;
    out.write_u64::<BigEndian>(PAYLOAD_MAJOR_VERSION)?;
    out.write_u64::<BigEndian>(manifest_bytes.len() as u64)?;
    out.write_u32::<BigEndian>(0)?; // metadata_signature_size = 0
    out.write_all(&manifest_bytes)?;
    out.write_all(&blobs)?;
    Ok(out)
}
