//! Program (de)serialization — the `.spq.bc` compiled-bytecode format.
//!
//! A compiled query is the constant pool, the code byte stream, and the debug
//! table. The statically-extracted `region`/optimizer metadata is **not**
//! serialized (it derives from the AST, which the bytecode does not carry); a
//! program loaded from bytecode therefore falls back to full-scan + per-record
//! predicate filtering — correct, just without the BAI-seek optimization.
//!
//! ```text
//! [0..4]  magic   b"SPQL"
//! [4]     version 0x01
//! [5..9]  const count (u32 le)
//! [9..]   consts: per entry (u8 tag, data)
//!           0x01 Int   (i64 le)
//!           0x02 Float (f64 le)
//!           0x03 Str   (u32 len le, utf8 bytes)
//!           0x04 Bool  (u8 0/1)
//!           0x05 Null
//! [..]    code count (u32 le) then code bytes
//! [..]    debug count (u32 le) then entries (u32 code_offset, u32 start, u32 end)
//! ```

use std::rc::Rc;
use std::str::Utf8Error;

use spliceql::token::Span;

use crate::compiler::{DebugInfo, Program, Value};

const MAGIC: &[u8; 4] = b"SPQL";
const VERSION: u8 = 0x01;

/// An error decoding a `.spq.bc` buffer.
#[derive(Debug)]
pub enum BytecodeError {
    InvalidMagic,
    UnsupportedVersion(u8),
    Truncated,
    InvalidUtf8(Utf8Error),
}

impl std::fmt::Display for BytecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BytecodeError::InvalidMagic => write!(f, "not a SPQL bytecode file (bad magic)"),
            BytecodeError::UnsupportedVersion(v) => write!(f, "unsupported bytecode version {v}"),
            BytecodeError::Truncated => write!(f, "bytecode is truncated"),
            BytecodeError::InvalidUtf8(e) => write!(f, "invalid utf-8 in constant: {e}"),
        }
    }
}

impl std::error::Error for BytecodeError {}

impl Program {
    /// Serialize this program to the `.spq.bc` byte format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.push(VERSION);

        out.extend_from_slice(&(self.consts.len() as u32).to_le_bytes());
        for c in &self.consts {
            match c {
                Value::Int(n) => {
                    out.push(0x01);
                    out.extend_from_slice(&n.to_le_bytes());
                }
                Value::Float(x) => {
                    out.push(0x02);
                    out.extend_from_slice(&x.to_le_bytes());
                }
                Value::Str(s) => {
                    out.push(0x03);
                    let bytes = s.as_bytes();
                    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                    out.extend_from_slice(bytes);
                }
                Value::Bool(b) => {
                    out.push(0x04);
                    out.push(*b as u8);
                }
                Value::Null => out.push(0x05),
            }
        }

        out.extend_from_slice(&(self.code.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.code);

        out.extend_from_slice(&(self.debug.len() as u32).to_le_bytes());
        for d in &self.debug {
            out.extend_from_slice(&(d.code_offset as u32).to_le_bytes());
            out.extend_from_slice(&(d.span.start as u32).to_le_bytes());
            out.extend_from_slice(&(d.span.end as u32).to_le_bytes());
        }
        out
    }

    /// Deserialize a program from the `.spq.bc` byte format.
    pub fn from_bytes(b: &[u8]) -> Result<Program, BytecodeError> {
        let mut r = Reader { b, pos: 0 };
        if r.take(4)? != MAGIC {
            return Err(BytecodeError::InvalidMagic);
        }
        let version = r.u8()?;
        if version != VERSION {
            return Err(BytecodeError::UnsupportedVersion(version));
        }

        let nconsts = r.u32()? as usize;
        let mut consts = Vec::with_capacity(nconsts);
        for _ in 0..nconsts {
            let tag = r.u8()?;
            let v = match tag {
                0x01 => Value::Int(i64::from_le_bytes(r.array8()?)),
                0x02 => Value::Float(f64::from_le_bytes(r.array8()?)),
                0x03 => {
                    let len = r.u32()? as usize;
                    let bytes = r.take(len)?;
                    let s = std::str::from_utf8(bytes).map_err(BytecodeError::InvalidUtf8)?;
                    Value::Str(Rc::from(s))
                }
                0x04 => Value::Bool(r.u8()? != 0),
                0x05 => Value::Null,
                _ => return Err(BytecodeError::Truncated),
            };
            consts.push(v);
        }

        let ncode = r.u32()? as usize;
        let code = r.take(ncode)?.to_vec();

        let ndebug = r.u32()? as usize;
        let mut debug = Vec::with_capacity(ndebug);
        for _ in 0..ndebug {
            let code_offset = r.u32()? as usize;
            let start = r.u32()? as usize;
            let end = r.u32()? as usize;
            debug.push(DebugInfo {
                code_offset,
                span: Span::new(start, end),
            });
        }

        Ok(Program {
            consts,
            code,
            debug,
            region: None, // not carried in bytecode (see module docs)
        })
    }
}

/// A bounds-checked cursor over the bytecode buffer.
struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], BytecodeError> {
        let end = self.pos.checked_add(n).ok_or(BytecodeError::Truncated)?;
        if end > self.b.len() {
            return Err(BytecodeError::Truncated);
        }
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, BytecodeError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, BytecodeError> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn array8(&mut self) -> Result<[u8; 8], BytecodeError> {
        let s = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(a)
    }
}
