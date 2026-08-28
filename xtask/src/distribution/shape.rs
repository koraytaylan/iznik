//! What an artifact's own headers say about it: the machine it is for, whether
//! it names a loader, and whether it still carries a symbol table.
//!
//! The headers, not the strings. musl's loader path appears in the data of a
//! statically linked binary, so searching for it would call every artifact
//! dynamic; a library path that happens to appear in a Mach-O file's data is
//! not something it links. Both readers walk the tables the format defines and
//! refuse a file whose tables they cannot walk, because reporting a truncated
//! file as static and stripped is exactly the wrong way round.
//!
//! It lives in the library rather than beside either artifact case because
//! both need it, and because what these readers say is worth proving against
//! files made to have each shape — which no build of this workspace produces.

use core::fmt::{self, Display, Formatter};

/// What an ELF file's headers say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ElfShape {
    /// The machine it declares.
    pub machine: u16,
    /// Whether any program header names an interpreter, which a statically
    /// linked executable has none of.
    pub interpreted: bool,
    /// Whether any section is a symbol table, which a stripped one has none
    /// of.
    pub symbols: bool,
}

/// What a Mach-O file's header and load commands say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachShape {
    /// The CPU type it declares.
    pub cpu: u32,
    /// Every library it names, which is its whole dynamic-link surface.
    pub linked: Vec<String>,
}

/// Why a file could not be read as the shape it was meant to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShapeError {
    /// It does not begin the way that format begins.
    NotTheFormat {
        /// What it was read as.
        format: &'static str,
    },
    /// It ends before something the format requires.
    Truncated {
        /// What was being read when it ran out.
        wanted: &'static str,
    },
    /// A table this must walk cannot be walked.
    Unwalkable {
        /// What was being walked.
        table: &'static str,
        /// What it said about itself.
        detail: String,
    },
}

impl Display for ShapeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            ShapeError::NotTheFormat { format } => write!(formatter, "not a {format} file"),
            ShapeError::Truncated { wanted } => write!(formatter, "it ends before {wanted}"),
            ShapeError::Unwalkable { table, detail } => {
                write!(formatter, "its {table} cannot be walked: {detail}")
            }
        }
    }
}

impl core::error::Error for ShapeError {}

/// What an ELF file begins with.
const ELF_MAGIC: &[u8] = b"\x7fELF";

/// Where the machine field is in an ELF header.
const MACHINE_AT: usize = 18;

/// Where the program header table's offset is.
const PROGRAM_HEADERS_AT: usize = 32;

/// Where the section header table's offset is.
const SECTION_HEADERS_AT: usize = 40;

/// Where the size of one program header entry is.
const PROGRAM_SIZE_AT: usize = 54;

/// Where the count of them is.
const PROGRAM_COUNT_AT: usize = 56;

/// Where the size of one section header entry is.
const SECTION_SIZE_AT: usize = 58;

/// Where the count of them is.
const SECTION_COUNT_AT: usize = 60;

/// Where a program header's type is.
const PROGRAM_KIND_AT: usize = 0;

/// Where a section header's type is. Not the same place: an ELF64 program
/// header carries its flags where a section header carries its type, so one
/// offset for both would read a segment's permissions and find a loader in
/// nothing.
const SECTION_KIND_AT: usize = 4;

/// The program header type of an interpreter: what a dynamically linked
/// executable names its loader with, and a static one has none of.
pub const PROGRAM_INTERPRETER: u32 = 3;

/// The section type of a symbol table: what stripping takes out, and the one
/// thing that says whether it happened.
pub const SECTION_SYMBOLS: u32 = 2;

/// What a 64-bit Mach-O file begins with, little-endian.
const MACH_MAGIC: u32 = 0xfeed_facf;

/// Where a Mach-O header's CPU type is.
const CPU_AT: usize = 4;

/// Where the count of load commands is.
const COMMAND_COUNT_AT: usize = 16;

/// How long a 64-bit Mach-O header is, and so where its load commands start.
const COMMANDS_AT: usize = 32;

/// Where a load command's length is, in every load command.
const COMMAND_SIZE_AT: usize = 4;

/// Where, within a library command, the offset of its name is.
const NAME_OFFSET_AT: usize = 8;

/// The load command that names a library the file needs at run time.
pub const LOAD_DYLIB: u32 = 0x0000_000c;

/// It and the two of the same shape beside it: a weak dependency and a
/// re-export.
const LOAD_LIBRARY: &[u32] = &[LOAD_DYLIB, 0x8000_0018, 0x8000_001f];

/// A little-endian half at `at`, if the bytes reach that far.
fn half(bytes: &[u8], at: usize) -> Option<u16> {
    bytes
        .get(at..at.saturating_add(HALF_LENGTH))
        .and_then(|held| <[u8; HALF_LENGTH]>::try_from(held).ok())
        .map(u16::from_le_bytes)
}

/// A little-endian word at `at`, if the bytes reach that far.
fn word(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at.saturating_add(WORD_LENGTH))
        .and_then(|held| <[u8; WORD_LENGTH]>::try_from(held).ok())
        .map(u32::from_le_bytes)
}

/// How many bytes a half is: the width of a count or a size in an ELF header.
const HALF_LENGTH: usize = 2;

/// How many bytes a word is: the width of a type in either format.
const WORD_LENGTH: usize = 4;

/// How many bytes an offset in either format is.
const OFFSET_LENGTH: usize = 8;

/// A little-endian offset at `at`, if the bytes reach that far.
fn offset(bytes: &[u8], at: usize) -> Option<usize> {
    bytes
        .get(at..at.saturating_add(OFFSET_LENGTH))
        .and_then(|held| <[u8; OFFSET_LENGTH]>::try_from(held).ok())
        .map(u64::from_le_bytes)
        .and_then(|held| usize::try_from(held).ok())
}

/// The nul-terminated name at `at`, if there is one and it is terminated.
///
/// A run of bytes to the end is not a name: a file that stopped mid-string is
/// truncated, and reporting the rest of it as a library would be an answer
/// where there should be a refusal.
fn name(bytes: &[u8], at: usize) -> Option<String> {
    let rest = bytes.get(at..)?;
    let end = rest.iter().position(|byte| *byte == 0)?;
    rest.get(..end)
        .map(|held| String::from_utf8_lossy(held).into_owned())
}

/// Whether any entry of a header table declares `kind`.
///
/// A table this cannot walk — no entries, entries of no length, or a header
/// past the end — is an error rather than an answer of `false`.
///
/// # Errors
///
/// [`ShapeError::Unwalkable`] when the table cannot be walked.
fn declares(
    bytes: &[u8],
    table: &'static str,
    at: usize,
    stride: usize,
    count: usize,
    kind_at: usize,
    kind: u32,
) -> Result<bool, ShapeError> {
    if stride == 0 || count == 0 {
        return Err(ShapeError::Unwalkable {
            table,
            detail: format!("{count} entries of {stride} bytes"),
        });
    }
    let mut found = false;
    for index in 0..count {
        let entry = at
            .checked_add(index.saturating_mul(stride))
            .ok_or_else(|| ShapeError::Unwalkable {
                table,
                detail: "it runs past what can be addressed".to_owned(),
            })?;
        let declared =
            word(bytes, entry.saturating_add(kind_at)).ok_or_else(|| ShapeError::Unwalkable {
                table,
                detail: format!("entry {index} is past the end of the file"),
            })?;
        found = found || declared == kind;
    }
    Ok(found)
}

/// What an ELF file's own headers say.
///
/// # Errors
///
/// [`ShapeError`] when the bytes are not an ELF file, end before a field, or
/// carry a header table that cannot be walked.
pub fn elf_shape(bytes: &[u8]) -> Result<ElfShape, ShapeError> {
    if bytes.get(0..ELF_MAGIC.len()) != Some(ELF_MAGIC) {
        return Err(ShapeError::NotTheFormat { format: "an ELF" });
    }
    let machine = half(bytes, MACHINE_AT).ok_or(ShapeError::Truncated {
        wanted: "its machine",
    })?;
    let programs = offset(bytes, PROGRAM_HEADERS_AT).ok_or(ShapeError::Truncated {
        wanted: "its program headers",
    })?;
    let interpreted = declares(
        bytes,
        "program header table",
        programs,
        usize::from(half(bytes, PROGRAM_SIZE_AT).unwrap_or_default()),
        usize::from(half(bytes, PROGRAM_COUNT_AT).unwrap_or_default()),
        PROGRAM_KIND_AT,
        PROGRAM_INTERPRETER,
    )?;
    let sections = offset(bytes, SECTION_HEADERS_AT).ok_or(ShapeError::Truncated {
        wanted: "its section headers",
    })?;
    let symbols = declares(
        bytes,
        "section header table",
        sections,
        usize::from(half(bytes, SECTION_SIZE_AT).unwrap_or_default()),
        usize::from(half(bytes, SECTION_COUNT_AT).unwrap_or_default()),
        SECTION_KIND_AT,
        SECTION_SYMBOLS,
    )?;
    Ok(ElfShape {
        machine,
        interpreted,
        symbols,
    })
}

/// What a 64-bit Mach-O file's header and load commands say.
///
/// # Errors
///
/// [`ShapeError`] when the bytes are not a 64-bit Mach-O file, end before a
/// field, or carry load commands that run past their end.
pub fn mach_shape(bytes: &[u8]) -> Result<MachShape, ShapeError> {
    if word(bytes, 0) != Some(MACH_MAGIC) {
        return Err(ShapeError::NotTheFormat {
            format: "a 64-bit Mach-O",
        });
    }
    let cpu = word(bytes, CPU_AT).ok_or(ShapeError::Truncated {
        wanted: "its CPU type",
    })?;
    let count = word(bytes, COMMAND_COUNT_AT).ok_or(ShapeError::Truncated {
        wanted: "its command count",
    })?;
    let mut linked = Vec::new();
    let mut at = COMMANDS_AT;
    for index in 0..count {
        let unwalkable = |detail: String| ShapeError::Unwalkable {
            table: "load commands",
            detail,
        };
        let kind = word(bytes, at)
            .ok_or_else(|| unwalkable(format!("command {index} begins past the end")))?;
        let size = word(bytes, at.saturating_add(COMMAND_SIZE_AT))
            .and_then(|held| usize::try_from(held).ok())
            .filter(|held| *held > 0)
            .ok_or_else(|| unwalkable(format!("command {index} declares no length")))?;
        if LOAD_LIBRARY.contains(&kind) {
            let named = word(bytes, at.saturating_add(NAME_OFFSET_AT))
                .and_then(|held| usize::try_from(held).ok())
                .and_then(|held| at.checked_add(held))
                .ok_or_else(|| unwalkable(format!("command {index} names nothing")))?;
            linked
                .push(name(bytes, named).ok_or_else(|| {
                    unwalkable(format!("command {index}'s name is unterminated"))
                })?);
        }
        at = at.saturating_add(size);
    }
    Ok(MachShape { cpu, linked })
}
