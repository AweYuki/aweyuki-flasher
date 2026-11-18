use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;

/// Thread-safe random-access byte source (works identically for Files on Native/Termux
/// and `Vec<u8>` / JS buffers in WebAssembly).
pub trait RandomAccessSource: Send + Sync {
    fn len(&self) -> u64;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;
}

impl RandomAccessSource for Vec<u8> {
    fn len(&self) -> u64 {
        Vec::len(self) as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let start = offset as usize;
        if start >= self.len() {
            return Ok(0);
        }
        let avail = &self[start..];
        let n = avail.len().min(buf.len());
        buf[..n].copy_from_slice(&avail[..n]);
        Ok(n)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub struct FileSource {
    file: std::fs::File,
    len: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileSource {
    pub fn open(path: impl AsRef<std::path::Path>) -> io::Result<Self> {
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file, len })
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl RandomAccessSource for FileSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            self.file.read_at(buf, offset)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            self.file.seek_read(buf, offset)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (offset, buf);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "random-access file read unsupported on this target",
            ))
        }
    }
}

/// Read + Seek cursor over any `Arc<dyn RandomAccessSource>`.
#[derive(Clone)]
pub struct SourceReader {
    source: Arc<dyn RandomAccessSource>,
    base_offset: u64,
    window_len: u64,
    pos: u64,
}

impl SourceReader {
    pub fn new(source: Arc<dyn RandomAccessSource>) -> Self {
        let len = source.len();
        Self {
            source,
            base_offset: 0,
            window_len: len,
            pos: 0,
        }
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self::new(Arc::new(bytes))
    }

    /// Create a zero-copy sub-window (e.g. for an uncompressed payload.bin inside a ZIP).
    pub fn slice(&self, offset: u64, len: u64) -> io::Result<Self> {
        if offset.saturating_add(len) > self.window_len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "slice exceeds source length",
            ));
        }
        Ok(Self {
            source: Arc::clone(&self.source),
            base_offset: self.base_offset + offset,
            window_len: len,
            pos: 0,
        })
    }

    pub fn len(&self) -> u64 {
        self.window_len
    }

    pub fn is_empty(&self) -> bool {
        self.window_len == 0
    }

    pub fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if offset.saturating_add(buf.len() as u64) > self.window_len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "read_exact_at past window end",
            ));
        }
        let mut filled = 0;
        while filled < buf.len() {
            let n = self
                .source
                .read_at(self.base_offset + offset + filled as u64, &mut buf[filled..])?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unexpected EOF during read_exact_at",
                ));
            }
            filled += n;
        }
        Ok(())
    }

    pub fn read_vec_at(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut out = vec![0u8; len];
        self.read_exact_at(offset, &mut out)?;
        Ok(out)
    }

    pub fn read_all(&self) -> io::Result<Vec<u8>> {
        let len = usize::try_from(self.window_len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source too large for RAM"))?;
        self.read_vec_at(0, len)
    }
}

impl Read for SourceReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.window_len {
            return Ok(0);
        }
        let max_read = (self.window_len - self.pos).min(buf.len() as u64) as usize;
        let n = self
            .source
            .read_at(self.base_offset + self.pos, &mut buf[..max_read])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for SourceReader {
    fn seek(&mut self, style: SeekFrom) -> io::Result<u64> {
        let new_pos: i64 = match style {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::End(delta) => (self.window_len as i64) + delta,
            SeekFrom::Current(delta) => (self.pos as i64) + delta,
        };
        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot seek before position 0",
            ));
        }
        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}
