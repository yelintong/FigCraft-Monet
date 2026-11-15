//! Bounds-checked big-endian readers and writers for the ABR binary structures.
//!
//! Every length is checked before it is read: a truncated or deliberately hostile `.abr` file
//! produces an error (or a warning) instead of a panic. All numbers in an ABR file are big-endian.

pub(crate) type Result<T> = std::result::Result<T, String>;

pub(crate) struct Reader<'a> {
    pub b: &'a [u8],
    pub p: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Self { b, p: 0 }
    }

    pub fn left(&self) -> usize {
        self.b.len().saturating_sub(self.p)
    }

    pub fn pos(&self) -> usize {
        self.p
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.p.checked_add(n).ok_or_else(|| "abr length overflow".to_string())?;
        let s = self.b.get(self.p..end).ok_or_else(|| format!("abr data ends at byte {} (needed {})", self.b.len(), end))?;
        self.p = end;
        Ok(s)
    }

    pub fn arr<const N: usize>(&mut self) -> Result<[u8; N]> {
        let s = self.take(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.arr::<1>()?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.arr()?))
    }

    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.arr()?))
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.arr()?))
    }

    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.arr()?))
    }

    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_be_bytes(self.arr()?))
    }

    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }

    /// Skip a class header (`UnicodeString` + `Id`) inside an `obj ` reference item.
    pub fn skip_unicode_and_id(&mut self) -> Result<()> {
        let units = self.u32()? as usize;
        self.take(units.saturating_mul(2))?;
        let n = self.u32()? as usize;
        self.take(if n == 0 { 4 } else { n })?;
        Ok(())
    }

    /// A Photoshop Unicode string: `u32` count of UTF-16 units, then the units, usually with a
    /// trailing NUL. Trailing NULs are stripped here (the ABR keys are compared as plain strings).
    pub fn unicode(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        if n > 1 << 20 {
            return Err("abr unicode string is too long".to_string());
        }
        let bytes = self.take(n.saturating_mul(2))?;
        let mut units = Vec::with_capacity(n);
        for c in bytes.chunks_exact(2) {
            units.push(u16::from_be_bytes([c[0], c[1]]));
        }
        while units.last() == Some(&0) {
            units.pop();
        }
        Ok(String::from_utf16_lossy(&units))
    }
}

pub(crate) struct Writer {
    pub b: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self { b: Vec::new() }
    }

    pub fn u8(&mut self, v: u8) {
        self.b.push(v);
    }

    pub fn u16(&mut self, v: u16) {
        self.b.extend_from_slice(&v.to_be_bytes());
    }

    pub fn u32(&mut self, v: u32) {
        self.b.extend_from_slice(&v.to_be_bytes());
    }

    pub fn i32(&mut self, v: i32) {
        self.b.extend_from_slice(&v.to_be_bytes());
    }

    pub fn i64(&mut self, v: i64) {
        self.b.extend_from_slice(&v.to_be_bytes());
    }

    pub fn f64(&mut self, v: f64) {
        self.b.extend_from_slice(&v.to_be_bytes());
    }

    pub fn bytes(&mut self, v: &[u8]) {
        self.b.extend_from_slice(v);
    }

    /// A NUL-terminated UTF-16 string, the shape Photoshop writes in descriptors.
    pub fn unicode(&mut self, s: &str) {
        let units: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        self.u32(units.len() as u32);
        for u in units {
            self.u16(u);
        }
    }
}
