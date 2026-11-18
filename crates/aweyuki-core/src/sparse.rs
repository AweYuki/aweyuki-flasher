use crate::error::{FlashError, Result};
use crate::reader::SourceReader;
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use serde::{Deserialize, Serialize};
use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};

pub const SPARSE_HEADER_MAGIC: u32 = 0xED26_FF3A;
pub const SPARSE_HEADER_SIZE: u16 = 28;
pub const CHUNK_HEADER_SIZE: u16 = 12;

pub const CHUNK_TYPE_RAW: u16 = 0xCAC1;
pub const CHUNK_TYPE_FILL: u16 = 0xCAC2;
pub const CHUNK_TYPE_DONT_CARE: u16 = 0xCAC3;
pub const CHUNK_TYPE_CRC32: u16 = 0xCAC4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparseHeader {
    pub magic: u32,
    pub major_version: u16,
    pub minor_version: u16,
    pub file_hdr_sz: u16,
    pub chunk_hdr_sz: u16,
    pub blk_sz: u32,
    pub total_blks: u32,
    pub total_chunks: u32,
    pub image_checksum: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkHeader {
    pub chunk_type: u16,
    pub reserved1: u16,
    pub chunk_sz: u32,
    pub total_sz: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparseChunkDescriptor {
    pub chunk_type: u16,
    pub start_block: u32,
    pub block_count: u32,
    pub file_data_offset: u64,
    pub data_bytes: u64,
    pub fill_value: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparseInfo {
    pub header: SparseHeader,
    pub raw_size: u64,
    pub sparse_file_size: u64,
    pub chunks: Vec<SparseChunkDescriptor>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum VisualSegmentKind {
    DontCareLead,
    Raw,
    FillZero,
    DontCareMiddle,
    DontCareTail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualSegment {
    pub kind: VisualSegmentKind,
    pub start_block: u32,
    pub block_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparseSplitMeta {
    pub index: usize,
    pub total_parts: usize,
    pub start_block: u32,
    pub active_blocks: u32,
    pub total_blocks: u32,
    pub encoded_bytes: usize,
    pub segments: Vec<VisualSegment>,
}

#[derive(Debug, Clone)]
pub struct SparseChunkBlob {
    pub meta: SparseSplitMeta,
    pub data: Vec<u8>,
}

pub fn is_sparse_magic(prefix: &[u8]) -> bool {
    prefix.len() >= 4 && u32::from_le_bytes([prefix[0], prefix[1], prefix[2], prefix[3]]) == SPARSE_HEADER_MAGIC
}

pub fn read_sparse_header<R: Read>(r: &mut R) -> Result<SparseHeader> {
    let magic = r.read_u32::<LittleEndian>()?;
    if magic != SPARSE_HEADER_MAGIC {
        return Err(FlashError::InvalidFormat(format!(
            "Invalid sparse magic: 0x{magic:08x}"
        )));
    }
    let major_version = r.read_u16::<LittleEndian>()?;
    let minor_version = r.read_u16::<LittleEndian>()?;
    let file_hdr_sz = r.read_u16::<LittleEndian>()?;
    let chunk_hdr_sz = r.read_u16::<LittleEndian>()?;
    let blk_sz = r.read_u32::<LittleEndian>()?;
    let total_blks = r.read_u32::<LittleEndian>()?;
    let total_chunks = r.read_u32::<LittleEndian>()?;
    let image_checksum = r.read_u32::<LittleEndian>()?;

    if major_version != 1 {
        return Err(FlashError::Unsupported(format!(
            "Unsupported sparse major version {major_version}.{minor_version}"
        )));
    }
    if file_hdr_sz < SPARSE_HEADER_SIZE || chunk_hdr_sz < CHUNK_HEADER_SIZE {
        return Err(FlashError::InvalidFormat(
            "Sparse header size smaller than specification minimum".into(),
        ));
    }
    if blk_sz == 0 || (blk_sz % 4) != 0 {
        return Err(FlashError::InvalidFormat(format!(
            "Invalid sparse block size: {blk_sz}"
        )));
    }

    if file_hdr_sz > SPARSE_HEADER_SIZE {
        let extra = (file_hdr_sz - SPARSE_HEADER_SIZE) as usize;
        let mut skip = vec![0u8; extra];
        r.read_exact(&mut skip)?;
    }

    Ok(SparseHeader {
        magic,
        major_version,
        minor_version,
        file_hdr_sz,
        chunk_hdr_sz,
        blk_sz,
        total_blks,
        total_chunks,
        image_checksum,
    })
}

pub fn write_sparse_header<W: Write>(w: &mut W, blk_sz: u32, total_blks: u32, total_chunks: u32) -> io::Result<()> {
    w.write_u32::<LittleEndian>(SPARSE_HEADER_MAGIC)?;
    w.write_u16::<LittleEndian>(1)?;
    w.write_u16::<LittleEndian>(0)?;
    w.write_u16::<LittleEndian>(SPARSE_HEADER_SIZE)?;
    w.write_u16::<LittleEndian>(CHUNK_HEADER_SIZE)?;
    w.write_u32::<LittleEndian>(blk_sz)?;
    w.write_u32::<LittleEndian>(total_blks)?;
    w.write_u32::<LittleEndian>(total_chunks)?;
    w.write_u32::<LittleEndian>(0)?;
    Ok(())
}

pub fn write_chunk_header<W: Write>(
    w: &mut W,
    chunk_type: u16,
    chunk_sz: u32,
    total_sz: u32,
) -> io::Result<()> {
    w.write_u16::<LittleEndian>(chunk_type)?;
    w.write_u16::<LittleEndian>(0)?;
    w.write_u32::<LittleEndian>(chunk_sz)?;
    w.write_u32::<LittleEndian>(total_sz)?;
    Ok(())
}

fn read_chunk_header<R: Read>(r: &mut R, chunk_hdr_sz: u16) -> Result<ChunkHeader> {
    let chunk_type = r.read_u16::<LittleEndian>()?;
    let reserved1 = r.read_u16::<LittleEndian>()?;
    let chunk_sz = r.read_u32::<LittleEndian>()?;
    let total_sz = r.read_u32::<LittleEndian>()?;
    if chunk_hdr_sz > CHUNK_HEADER_SIZE {
        let extra = (chunk_hdr_sz - CHUNK_HEADER_SIZE) as usize;
        let mut skip = vec![0u8; extra];
        r.read_exact(&mut skip)?;
    }
    if total_sz < chunk_hdr_sz as u32 {
        return Err(FlashError::InvalidFormat(format!(
            "Sparse chunk total_sz ({total_sz}) < chunk_hdr_sz ({chunk_hdr_sz})"
        )));
    }
    Ok(ChunkHeader {
        chunk_type,
        reserved1,
        chunk_sz,
        total_sz,
    })
}

pub fn inspect_sparse(reader: &mut SourceReader) -> Result<SparseInfo> {
    reader.seek(SeekFrom::Start(0))?;
    let sparse_file_size = reader.len();
    let header = read_sparse_header(reader)?;
    let mut chunks = Vec::with_capacity(header.total_chunks as usize);
    let mut cur_blk: u32 = 0;

    for _ in 0..header.total_chunks {
        let ch = read_chunk_header(reader, header.chunk_hdr_sz)?;
        let payload_len = (ch.total_sz - header.chunk_hdr_sz as u32) as u64;
        let file_data_offset = reader.stream_position()?;
        let fill_value = match ch.chunk_type {
            CHUNK_TYPE_RAW => {
                let expected = (ch.chunk_sz as u64) * (header.blk_sz as u64);
                if payload_len != expected {
                    return Err(FlashError::InvalidFormat(format!(
                        "RAW chunk payload_len {payload_len} != expected {expected}"
                    )));
                }
                reader.seek(SeekFrom::Current(payload_len as i64))?;
                None
            }
            CHUNK_TYPE_FILL => {
                if payload_len != 4 {
                    return Err(FlashError::InvalidFormat(
                        "FILL chunk payload must be 4 bytes".into(),
                    ));
                }
                Some(reader.read_u32::<LittleEndian>()?)
            }
            CHUNK_TYPE_DONT_CARE => {
                if payload_len != 0 {
                    reader.seek(SeekFrom::Current(payload_len as i64))?;
                }
                None
            }
            CHUNK_TYPE_CRC32 => {
                if payload_len != 4 {
                    return Err(FlashError::InvalidFormat(
                        "CRC32 chunk payload must be 4 bytes".into(),
                    ));
                }
                reader.seek(SeekFrom::Current(4))?;
                None
            }
            other => {
                return Err(FlashError::InvalidFormat(format!(
                    "Unknown sparse chunk type: 0x{other:04x}"
                )));
            }
        };

        chunks.push(SparseChunkDescriptor {
            chunk_type: ch.chunk_type,
            start_block: cur_blk,
            block_count: ch.chunk_sz,
            file_data_offset,
            data_bytes: payload_len,
            fill_value,
        });
        cur_blk = cur_blk.saturating_add(ch.chunk_sz);
    }

    let raw_size = (header.total_blks as u64) * (header.blk_sz as u64);
    Ok(SparseInfo {
        header,
        raw_size,
        sparse_file_size,
        chunks,
    })
}

pub fn sparse_to_raw<W: Write>(reader: &mut SourceReader, writer: &mut W) -> Result<u64> {
    let info = inspect_sparse(reader)?;
    let blk_sz = info.header.blk_sz as usize;
    let zero_block = vec![0u8; blk_sz];
    let mut copy_buf = vec![0u8; 64 * 1024];
    let mut written: u64 = 0;

    for chunk in &info.chunks {
        match chunk.chunk_type {
            CHUNK_TYPE_RAW => {
                reader.seek(SeekFrom::Start(chunk.file_data_offset))?;
                let mut rem = chunk.data_bytes;
                while rem > 0 {
                    let take = rem.min(copy_buf.len() as u64) as usize;
                    reader.read_exact(&mut copy_buf[..take])?;
                    writer.write_all(&copy_buf[..take])?;
                    rem -= take as u64;
                    written += take as u64;
                }
            }
            CHUNK_TYPE_FILL => {
                let val = chunk.fill_value.unwrap_or(0).to_le_bytes();
                let mut fill_block = Vec::with_capacity(blk_sz);
                for _ in 0..(blk_sz / 4) {
                    fill_block.extend_from_slice(&val);
                }
                for _ in 0..chunk.block_count {
                    writer.write_all(&fill_block)?;
                    written += blk_sz as u64;
                }
            }
            CHUNK_TYPE_DONT_CARE => {
                for _ in 0..chunk.block_count {
                    writer.write_all(&zero_block)?;
                    written += blk_sz as u64;
                }
            }
            CHUNK_TYPE_CRC32 => {}
            _ => {}
        }
    }
    writer.flush()?;
    Ok(written)
}

/// Random-access adapter over either a Raw image or an Android Sparse image.
#[derive(Clone)]
pub enum ImageRandomReader {
    Raw(SourceReader),
    Sparse {
        source: SourceReader,
        info: SparseInfo,
    },
}

impl ImageRandomReader {
    pub fn open(mut source: SourceReader) -> Result<Self> {
        let mut magic = [0u8; 4];
        if source.len() >= 4 {
            source.read_exact_at(0, &mut magic)?;
            if is_sparse_magic(&magic) {
                let info = inspect_sparse(&mut source)?;
                return Ok(Self::Sparse { source, info });
            }
        }
        Ok(Self::Raw(source))
    }

    pub fn raw_len(&self) -> u64 {
        match self {
            Self::Raw(r) => r.len(),
            Self::Sparse { info, .. } => info.raw_size,
        }
    }

    pub fn is_sparse(&self) -> bool {
        matches!(self, Self::Sparse { .. })
    }

    pub fn read_exact_at(&self, raw_offset: u64, out: &mut [u8]) -> Result<()> {
        match self {
            Self::Raw(r) => {
                r.read_exact_at(raw_offset, out)?;
                Ok(())
            }
            Self::Sparse { source, info } => {
                let blk_sz = info.header.blk_sz as u64;
                if raw_offset.saturating_add(out.len() as u64) > info.raw_size {
                    return Err(FlashError::Io(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "read past end of sparse raw view",
                    )));
                }
                let mut pos = 0usize;
                while pos < out.len() {
                    let cur_raw = raw_offset + pos as u64;
                    let cur_blk = (cur_raw / blk_sz) as u32;
                    let idx = info
                        .chunks
                        .partition_point(|c| c.start_block + c.block_count <= cur_blk);
                    let chunk = info.chunks.get(idx).ok_or_else(|| {
                        FlashError::InvalidFormat("Sparse block map missing target block".into())
                    })?;
                    let chunk_raw_start = (chunk.start_block as u64) * blk_sz;
                    let chunk_raw_end =
                        chunk_raw_start + (chunk.block_count as u64) * blk_sz;
                    let avail_in_chunk = (chunk_raw_end - cur_raw) as usize;
                    let take = avail_in_chunk.min(out.len() - pos);
                    let in_chunk_off = cur_raw - chunk_raw_start;

                    match chunk.chunk_type {
                        CHUNK_TYPE_RAW => {
                            source.read_exact_at(
                                chunk.file_data_offset + in_chunk_off,
                                &mut out[pos..pos + take],
                            )?;
                        }
                        CHUNK_TYPE_FILL => {
                            let val = chunk.fill_value.unwrap_or(0).to_le_bytes();
                            let start_mod = (in_chunk_off % 4) as usize;
                            for i in 0..take {
                                out[pos + i] = val[(start_mod + i) % 4];
                            }
                        }
                        CHUNK_TYPE_DONT_CARE => {
                            out[pos..pos + take].fill(0);
                        }
                        _ => {
                            out[pos..pos + take].fill(0);
                        }
                    }
                    pos += take;
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone)]
enum PendingOp {
    Raw(Vec<u8>),
    FillZero(u32),
    DontCare(u32),
}

impl PendingOp {
    fn blocks(&self, blk_sz: u32) -> u32 {
        match self {
            Self::Raw(b) => (b.len() as u32) / blk_sz,
            Self::FillZero(n) | Self::DontCare(n) => *n,
        }
    }

    fn encoded_bytes(&self) -> usize {
        match self {
            Self::Raw(b) => CHUNK_HEADER_SIZE as usize + b.len(),
            Self::FillZero(_) => CHUNK_HEADER_SIZE as usize + 4,
            Self::DontCare(_) => CHUNK_HEADER_SIZE as usize,
        }
    }
}

fn flush_pending_ops(
    ops: Vec<PendingOp>,
    start_block: u32,
    total_blocks: u32,
    blk_sz: u32,
    index: usize,
) -> Result<SparseChunkBlob> {
    let active_blocks: u32 = ops.iter().map(|op| op.blocks(blk_sz)).sum();
    let end_block = start_block + active_blocks;
    let lead_dc = start_block;
    let tail_dc = total_blocks.saturating_sub(end_block);

    let total_chunks = (if lead_dc > 0 { 1 } else { 0 })
        + ops.len() as u32
        + (if tail_dc > 0 { 1 } else { 0 });

    let est_size = SPARSE_HEADER_SIZE as usize
        + (if lead_dc > 0 { CHUNK_HEADER_SIZE as usize } else { 0 })
        + ops.iter().map(PendingOp::encoded_bytes).sum::<usize>()
        + (if tail_dc > 0 { CHUNK_HEADER_SIZE as usize } else { 0 });

    let mut out = Cursor::new(Vec::with_capacity(est_size));
    write_sparse_header(&mut out, blk_sz, total_blocks, total_chunks)?;

    let mut segments = Vec::new();
    let mut cur_blk = 0u32;

    if lead_dc > 0 {
        write_chunk_header(&mut out, CHUNK_TYPE_DONT_CARE, lead_dc, CHUNK_HEADER_SIZE as u32)?;
        segments.push(VisualSegment {
            kind: VisualSegmentKind::DontCareLead,
            start_block: 0,
            block_count: lead_dc,
        });
        cur_blk += lead_dc;
    }

    for op in ops {
        let count = op.blocks(blk_sz);
        match op {
            PendingOp::Raw(bytes) => {
                let total_sz = CHUNK_HEADER_SIZE as u32 + bytes.len() as u32;
                write_chunk_header(&mut out, CHUNK_TYPE_RAW, count, total_sz)?;
                out.write_all(&bytes)?;
                segments.push(VisualSegment {
                    kind: VisualSegmentKind::Raw,
                    start_block: cur_blk,
                    block_count: count,
                });
            }
            PendingOp::FillZero(n) => {
                write_chunk_header(&mut out, CHUNK_TYPE_FILL, n, CHUNK_HEADER_SIZE as u32 + 4)?;
                out.write_u32::<LittleEndian>(0)?;
                segments.push(VisualSegment {
                    kind: VisualSegmentKind::FillZero,
                    start_block: cur_blk,
                    block_count: n,
                });
            }
            PendingOp::DontCare(n) => {
                write_chunk_header(&mut out, CHUNK_TYPE_DONT_CARE, n, CHUNK_HEADER_SIZE as u32)?;
                segments.push(VisualSegment {
                    kind: VisualSegmentKind::DontCareMiddle,
                    start_block: cur_blk,
                    block_count: n,
                });
            }
        }
        cur_blk += count;
    }

    if tail_dc > 0 {
        write_chunk_header(&mut out, CHUNK_TYPE_DONT_CARE, tail_dc, CHUNK_HEADER_SIZE as u32)?;
        segments.push(VisualSegment {
            kind: VisualSegmentKind::DontCareTail,
            start_block: cur_blk,
            block_count: tail_dc,
        });
    }

    let data = out.into_inner();
    Ok(SparseChunkBlob {
        meta: SparseSplitMeta {
            index,
            total_parts: 0,
            start_block,
            active_blocks,
            total_blocks,
            encoded_bytes: data.len(),
            segments,
        },
        data,
    })
}

/// Split a raw image stream into multiple standalone Android Sparse `.simg` files,
/// each strictly `<= max_download_size` bytes.
pub fn split_raw_to_sparse_chunks<R: Read>(
    reader: &mut R,
    raw_len: u64,
    blk_sz: u32,
    max_download_size: u64,
) -> Result<Vec<SparseChunkBlob>> {
    if blk_sz == 0 || (blk_sz % 4) != 0 {
        return Err(FlashError::InvalidFormat("Invalid block size".into()));
    }
    let min_budget =
        SPARSE_HEADER_SIZE as u64 + 3 * (CHUNK_HEADER_SIZE as u64) + blk_sz as u64;
    if max_download_size < min_budget {
        return Err(FlashError::SizeLimitExceeded(format!(
            "max_download_size ({max_download_size}) too small; minimum required is {min_budget} bytes"
        )));
    }
    if raw_len == 0 {
        return Ok(Vec::new());
    }

    let total_blocks = u32::try_from(raw_len.div_ceil(blk_sz as u64))
        .map_err(|_| FlashError::SizeLimitExceeded("Raw image exceeds 16TB sparse limit".into()))?;

    let mut blobs = Vec::new();
    let mut ops: Vec<PendingOp> = Vec::new();
    let mut chunk_start_blk: u32 = 0;
    let mut cur_blk: u32 = 0;
    let mut active_encoded_bytes: usize = 0;
    let mut block_buf = vec![0u8; blk_sz as usize];

    let mut rem_bytes = raw_len;
    while rem_bytes > 0 {
        let to_read = rem_bytes.min(blk_sz as u64) as usize;
        block_buf.fill(0);
        reader.read_exact(&mut block_buf[..to_read])?;
        rem_bytes -= to_read as u64;

        let is_zero = block_buf.iter().all(|&b| b == 0);

        let can_merge_with_last = match ops.last() {
            Some(PendingOp::FillZero(_)) if is_zero => true,
            Some(PendingOp::Raw(_)) if !is_zero => true,
            _ => false,
        };
        let extra_bytes = if is_zero {
            if can_merge_with_last {
                0
            } else {
                CHUNK_HEADER_SIZE as usize + 4
            }
        } else if can_merge_with_last {
            blk_sz as usize
        } else {
            CHUNK_HEADER_SIZE as usize + blk_sz as usize
        };

        let next_end_blk = cur_blk + 1;
        let overhead_for_current = SPARSE_HEADER_SIZE as usize
            + (if chunk_start_blk > 0 { CHUNK_HEADER_SIZE as usize } else { 0 })
            + (if next_end_blk < total_blocks {
                CHUNK_HEADER_SIZE as usize
            } else {
                0
            });

        if !ops.is_empty()
            && (overhead_for_current + active_encoded_bytes + extra_bytes) as u64 > max_download_size
        {
            let blob = flush_pending_ops(ops, chunk_start_blk, total_blocks, blk_sz, blobs.len())?;
            blobs.push(blob);
            ops = Vec::new();
            chunk_start_blk = cur_blk;
            active_encoded_bytes = 0;
        }

        if is_zero {
            match ops.last_mut() {
                Some(PendingOp::FillZero(n)) => *n += 1,
                _ => {
                    ops.push(PendingOp::FillZero(1));
                    active_encoded_bytes += CHUNK_HEADER_SIZE as usize + 4;
                }
            }
        } else {
            match ops.last_mut() {
                Some(PendingOp::Raw(buf)) => {
                    buf.extend_from_slice(&block_buf);
                    active_encoded_bytes += blk_sz as usize;
                }
                _ => {
                    ops.push(PendingOp::Raw(block_buf.clone()));
                    active_encoded_bytes += CHUNK_HEADER_SIZE as usize + blk_sz as usize;
                }
            }
        }
        cur_blk += 1;
    }

    if !ops.is_empty() {
        let blob = flush_pending_ops(ops, chunk_start_blk, total_blocks, blk_sz, blobs.len())?;
        blobs.push(blob);
    }

    let total_parts = blobs.len();
    for b in &mut blobs {
        b.meta.total_parts = total_parts;
    }
    Ok(blobs)
}

/// Re-split an existing Android Sparse image directly into multiple Sparse `.simg` files
/// each `<= max_download_size`, without inflating DONT_CARE or FILL regions into raw RAM.
pub fn resparse_chunks(
    reader: &mut SourceReader,
    max_download_size: u64,
) -> Result<Vec<SparseChunkBlob>> {
    let info = inspect_sparse(reader)?;
    let blk_sz = info.header.blk_sz;
    let total_blocks = info.header.total_blks;
    let min_budget =
        SPARSE_HEADER_SIZE as u64 + 3 * (CHUNK_HEADER_SIZE as u64) + blk_sz as u64;
    if max_download_size < min_budget {
        return Err(FlashError::SizeLimitExceeded(format!(
            "max_download_size ({max_download_size}) too small for sparse block size {blk_sz}"
        )));
    }

    let mut blobs = Vec::new();
    let mut ops: Vec<PendingOp> = Vec::new();
    let mut chunk_start_blk: u32 = 0;
    let mut cur_blk: u32 = 0;
    let mut active_encoded_bytes: usize = 0;

    let max_overhead = SPARSE_HEADER_SIZE as usize + 2 * (CHUNK_HEADER_SIZE as usize);

    for chunk in &info.chunks {
        match chunk.chunk_type {
            CHUNK_TYPE_CRC32 => continue,
            CHUNK_TYPE_DONT_CARE => {
                // Skip leading/trailing DONT_CARE if buffer is empty; inside active range keep it compact
                if ops.is_empty() {
                    cur_blk += chunk.block_count;
                    chunk_start_blk = cur_blk;
                } else {
                    let extra = CHUNK_HEADER_SIZE as usize;
                    if (max_overhead + active_encoded_bytes + extra) as u64 > max_download_size {
                        let blob = flush_pending_ops(
                            ops,
                            chunk_start_blk,
                            total_blocks,
                            blk_sz,
                            blobs.len(),
                        )?;
                        blobs.push(blob);
                        ops = Vec::new();
                        cur_blk += chunk.block_count;
                        chunk_start_blk = cur_blk;
                        active_encoded_bytes = 0;
                    } else {
                        ops.push(PendingOp::DontCare(chunk.block_count));
                        active_encoded_bytes += extra;
                        cur_blk += chunk.block_count;
                    }
                }
            }
            CHUNK_TYPE_FILL => {
                let fill_val = chunk.fill_value.unwrap_or(0);
                if fill_val == 0 {
                    let extra = CHUNK_HEADER_SIZE as usize + 4;
                    if !ops.is_empty()
                        && (max_overhead + active_encoded_bytes + extra) as u64 > max_download_size
                    {
                        let blob = flush_pending_ops(
                            ops,
                            chunk_start_blk,
                            total_blocks,
                            blk_sz,
                            blobs.len(),
                        )?;
                        blobs.push(blob);
                        ops = Vec::new();
                        chunk_start_blk = cur_blk;
                        active_encoded_bytes = 0;
                    }
                    ops.push(PendingOp::FillZero(chunk.block_count));
                    active_encoded_bytes += extra;
                    cur_blk += chunk.block_count;
                } else {
                    // Non-zero FILL expanded into Raw blocks so flush_pending_ops remains exact
                    let pattern = fill_val.to_le_bytes();
                    let mut fill_blk = Vec::with_capacity(blk_sz as usize);
                    for _ in 0..(blk_sz / 4) {
                        fill_blk.extend_from_slice(&pattern);
                    }
                    for _ in 0..chunk.block_count {
                        let extra = match ops.last() {
                            Some(PendingOp::Raw(_)) => blk_sz as usize,
                            _ => CHUNK_HEADER_SIZE as usize + blk_sz as usize,
                        };
                        if !ops.is_empty()
                            && (max_overhead + active_encoded_bytes + extra) as u64
                                > max_download_size
                        {
                            let blob = flush_pending_ops(
                                ops,
                                chunk_start_blk,
                                total_blocks,
                                blk_sz,
                                blobs.len(),
                            )?;
                            blobs.push(blob);
                            ops = Vec::new();
                            chunk_start_blk = cur_blk;
                            active_encoded_bytes = 0;
                        }
                        match ops.last_mut() {
                            Some(PendingOp::Raw(buf)) => {
                                buf.extend_from_slice(&fill_blk);
                                active_encoded_bytes += blk_sz as usize;
                            }
                            _ => {
                                ops.push(PendingOp::Raw(fill_blk.clone()));
                                active_encoded_bytes += CHUNK_HEADER_SIZE as usize + blk_sz as usize;
                            }
                        }
                        cur_blk += 1;
                    }
                }
            }
            CHUNK_TYPE_RAW => {
                let mut rem_blks = chunk.block_count;
                let mut file_off = chunk.file_data_offset;
                while rem_blks > 0 {
                    let header_cost = match ops.last() {
                        Some(PendingOp::Raw(_)) => 0usize,
                        _ => CHUNK_HEADER_SIZE as usize,
                    };
                    let used = max_overhead + active_encoded_bytes + header_cost;
                    let avail_bytes = (max_download_size as usize).saturating_sub(used);
                    let avail_blks = (avail_bytes / blk_sz as usize) as u32;
                    if avail_blks == 0 {
                        let blob = flush_pending_ops(
                            ops,
                            chunk_start_blk,
                            total_blocks,
                            blk_sz,
                            blobs.len(),
                        )?;
                        blobs.push(blob);
                        ops = Vec::new();
                        chunk_start_blk = cur_blk;
                        active_encoded_bytes = 0;
                        continue;
                    }
                    let take_blks = rem_blks.min(avail_blks);
                    let take_bytes = (take_blks as usize) * (blk_sz as usize);
                    let data = reader.read_vec_at(file_off, take_bytes)?;
                    match ops.last_mut() {
                        Some(PendingOp::Raw(buf)) => {
                            buf.extend_from_slice(&data);
                            active_encoded_bytes += take_bytes;
                        }
                        _ => {
                            ops.push(PendingOp::Raw(data));
                            active_encoded_bytes += CHUNK_HEADER_SIZE as usize + take_bytes;
                        }
                    }
                    file_off += take_bytes as u64;
                    rem_blks -= take_blks;
                    cur_blk += take_blks;
                }
            }
            _ => {}
        }
    }

    if !ops.is_empty() {
        let blob = flush_pending_ops(ops, chunk_start_blk, total_blocks, blk_sz, blobs.len())?;
        blobs.push(blob);
    }

    let total_parts = blobs.len();
    for b in &mut blobs {
        b.meta.total_parts = total_parts;
    }
    Ok(blobs)
}

/// Automatically split either a raw or sparse image if it exceeds `max_download_size`.
pub fn auto_split_image(
    mut reader: SourceReader,
    max_download_size: u64,
) -> Result<Vec<SparseChunkBlob>> {
    let mut magic = [0u8; 4];
    if reader.len() >= 4 {
        reader.read_exact_at(0, &mut magic)?;
        if is_sparse_magic(&magic) {
            return resparse_chunks(&mut reader, max_download_size);
        }
    }
    let total_len = reader.len();
    reader.seek(SeekFrom::Start(0))?;
    split_raw_to_sparse_chunks(&mut reader, total_len, 4096, max_download_size)
}
