//! The readers an artifact case leans on, held to files made to have each
//! shape.
//!
//! No build of this workspace produces a dynamically linked artifact, an
//! unstripped one, or a Mach-O file at all — so the two answers each artifact
//! case asserts can only be told apart against files made to have them. That
//! is what this is: a synthetic ELF file with an interpreter in one table and
//! a symbol table in the other, and a synthetic Mach-O file whose libraries
//! can be found only by stepping over commands of another kind and length.
//!
//! It runs in the ordinary gate, unlike the artifact cases it protects, because
//! it builds nothing.

use xtask::distribution::shape::{
    LOAD_DYLIB, PROGRAM_INTERPRETER, SECTION_SYMBOLS, elf_shape, mach_shape,
};

/// What an ELF file begins with.
const ELF_MAGIC: &[u8] = b"\x7fELF";

/// The identification bytes after the magic: 64-bit, little-endian, version
/// one.
const ELF_IDENTIFICATION: &[u8] = &[2, 1, 1];

/// How long the whole identification is, magic included.
const ELF_IDENTIFICATION_LENGTH: usize = 16;

/// The file type of an executable.
const ELF_EXECUTABLE: u16 = 2;

/// The ELF version every current file declares.
const ELF_VERSION: u32 = 1;

/// How long an ELF64 header is, and so where the tables after it start.
const ELF_HEADER_LENGTH: usize = 64;

/// How long one program header is.
const PROGRAM_HEADER_LENGTH: usize = 56;

/// How long one section header is.
const SECTION_HEADER_LENGTH: usize = 64;

/// The program header type of a loadable segment: what a file has instead of
/// an interpreter when it names no loader.
const PROGRAM_LOAD: u32 = 1;

/// The section type of ordinary contents: what a file has instead of a symbol
/// table when it has been stripped.
const SECTION_PROGRAM_BITS: u32 = 1;

/// The machine value of x86-64, which these files declare because they have
/// to declare something.
const MACHINE_X86_64: u16 = 62;

/// What a 64-bit Mach-O file begins with, little-endian.
const MACH_MAGIC: u32 = 0xfeed_facf;

/// The file type of an executable, which is what an artifact is.
const MACH_EXECUTABLE: u32 = 2;

/// The CPU type of an Apple silicon Mac.
const CPU_ARM64: u32 = 0x0100_000c;

/// A load command that names no library. A segment is most of what a real
/// Mach-O file's commands are, and stepping over them by the length they
/// declare is the part of the walk that can go wrong unnoticed.
const LOAD_SEGMENT: u32 = 0x0000_0019;

/// How long one of those is with no sections in it.
const SEGMENT_LENGTH: usize = 72;

/// How long the fixed part of a library command is, name offset included.
const DYLIB_FIXED_LENGTH: usize = 24;

/// What a load command's name is padded to, which is how a linker lays one out
/// and so what the walk has to step over.
const NAME_ALIGNMENT: usize = 4;

/// The libraries the synthetic Mach-O file names.
const NAMED_LIBRARIES: &[&str] = &["/usr/lib/libSystem.B.dylib", "/usr/lib/libiconv.2.dylib"];

/// An ELF64 executable whose program headers are `programs` and whose section
/// headers are `sections`, and nothing else.
fn elf(programs: &[u32], sections: &[u32]) -> Vec<u8> {
    let at = ELF_HEADER_LENGTH;
    let sections_at = at.saturating_add(programs.len().saturating_mul(PROGRAM_HEADER_LENGTH));
    let mut bytes = Vec::new();
    bytes.extend_from_slice(ELF_MAGIC);
    bytes.extend_from_slice(ELF_IDENTIFICATION);
    bytes.resize(ELF_IDENTIFICATION_LENGTH, 0);
    bytes.extend_from_slice(&ELF_EXECUTABLE.to_le_bytes());
    bytes.extend_from_slice(&MACHINE_X86_64.to_le_bytes());
    bytes.extend_from_slice(&ELF_VERSION.to_le_bytes());
    bytes.extend_from_slice(&0_u64.to_le_bytes());
    bytes.extend_from_slice(&u64::try_from(at).unwrap_or_default().to_le_bytes());
    bytes.extend_from_slice(&u64::try_from(sections_at).unwrap_or_default().to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&length(ELF_HEADER_LENGTH));
    bytes.extend_from_slice(&length(PROGRAM_HEADER_LENGTH));
    bytes.extend_from_slice(&length(programs.len()));
    bytes.extend_from_slice(&length(SECTION_HEADER_LENGTH));
    bytes.extend_from_slice(&length(sections.len()));
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    for kind in programs {
        // The type first. A section header carries its name there and its type
        // four bytes on, so a reader using one offset for both tables would
        // find no interpreter in any of these.
        let mut header = kind.to_le_bytes().to_vec();
        header.resize(PROGRAM_HEADER_LENGTH, 0);
        bytes.extend_from_slice(&header);
    }
    for kind in sections {
        // The name, then the type: the other way round.
        let mut header = PROGRAM_INTERPRETER.to_le_bytes().to_vec();
        header.extend_from_slice(&kind.to_le_bytes());
        header.resize(SECTION_HEADER_LENGTH, 0);
        bytes.extend_from_slice(&header);
    }
    bytes
}

/// A count or a size as the two little-endian bytes an ELF header holds it in.
fn length(held: usize) -> [u8; 2] {
    u16::try_from(held).unwrap_or_default().to_le_bytes()
}

/// One load command that names a library, laid out as a linker lays it out:
/// the fixed part, then the name, padded.
fn dylib_command(library: &str) -> Vec<u8> {
    let mut named = library.as_bytes().to_vec();
    named.push(0);
    while !named.len().is_multiple_of(NAME_ALIGNMENT) {
        named.push(0);
    }
    let size = DYLIB_FIXED_LENGTH.saturating_add(named.len());
    let mut command = LOAD_DYLIB.to_le_bytes().to_vec();
    command.extend_from_slice(&u32::try_from(size).unwrap_or_default().to_le_bytes());
    command.extend_from_slice(
        &u32::try_from(DYLIB_FIXED_LENGTH)
            .unwrap_or_default()
            .to_le_bytes(),
    );
    command.resize(DYLIB_FIXED_LENGTH, 0);
    command.extend_from_slice(&named);
    command
}

/// One load command that names nothing: a segment, all zeros after its kind
/// and its length.
fn segment_command() -> Vec<u8> {
    let mut command = LOAD_SEGMENT.to_le_bytes().to_vec();
    command.extend_from_slice(
        &u32::try_from(SEGMENT_LENGTH)
            .unwrap_or_default()
            .to_le_bytes(),
    );
    command.resize(SEGMENT_LENGTH, 0);
    command
}

/// A 64-bit Mach-O file whose load commands are, in order, what `libraries`
/// says: a name for one the file needs, and nothing for a segment.
fn mach(cpu: u32, libraries: &[Option<&str>]) -> Vec<u8> {
    let mut commands = Vec::new();
    for library in libraries {
        match *library {
            Some(named) => commands.extend_from_slice(&dylib_command(named)),
            None => commands.extend_from_slice(&segment_command()),
        }
    }
    let mut bytes = MACH_MAGIC.to_le_bytes().to_vec();
    bytes.extend_from_slice(&cpu.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&MACH_EXECUTABLE.to_le_bytes());
    bytes.extend_from_slice(
        &u32::try_from(libraries.len())
            .unwrap_or_default()
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u32::try_from(commands.len())
            .unwrap_or_default()
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&commands);
    bytes
}

/// # Panics
///
/// When the walk does not find an interpreter that is there, finds one that is
/// not, does not find a symbol table that is there, finds one that is not, or
/// accepts a file whose tables it cannot walk.
#[test]
fn elf_shape_reads_both_header_tables() {
    let dynamic = elf_shape(&elf(
        &[PROGRAM_LOAD, PROGRAM_INTERPRETER],
        &[SECTION_PROGRAM_BITS],
    ))
    .unwrap_or_else(|error| panic!("a synthetic ELF file was refused: {error}"));
    assert_eq!(dynamic.machine, MACHINE_X86_64, "the machine is read");
    assert!(dynamic.interpreted, "and a program header naming a loader");
    assert!(!dynamic.symbols, "and no symbol table where there is none");
    let unstripped = elf_shape(&elf(
        &[PROGRAM_LOAD],
        &[SECTION_PROGRAM_BITS, SECTION_SYMBOLS],
    ))
    .unwrap_or_else(|error| panic!("a synthetic ELF file was refused: {error}"));
    assert!(!unstripped.interpreted, "a file naming no loader is static");
    assert!(unstripped.symbols, "and a symbol table where there is one");
    assert!(
        elf_shape(&elf(&[], &[SECTION_PROGRAM_BITS])).is_err(),
        "a table of no entries is refused rather than answered"
    );
    assert!(
        elf_shape(b"not an ELF file at all").is_err(),
        "and so is something that is not an ELF file"
    );
}

/// # Panics
///
/// When the load-command walk does not find exactly the libraries a Mach-O
/// file names, or accepts one it cannot walk.
#[test]
fn mach_shape_reads_what_a_file_names() {
    let first = NAMED_LIBRARIES.first().copied().unwrap_or_default();
    let second = NAMED_LIBRARIES.get(1).copied().unwrap_or_default();
    // Segments around and between them, so the two names can be found only by
    // stepping over commands of another kind and another length.
    let file = mach(
        CPU_ARM64,
        &[None, Some(first), None, None, Some(second), None],
    );
    let shape = mach_shape(&file)
        .unwrap_or_else(|error| panic!("a synthetic Mach-O file was refused: {error}"));
    assert_eq!(shape.cpu, CPU_ARM64, "the CPU type is read from the header");
    assert_eq!(shape.linked, NAMED_LIBRARIES, "and the libraries, in order");
    let none = mach_shape(&mach(CPU_ARM64, &[None, None]))
        .unwrap_or_else(|error| panic!("a file of segments was refused: {error}"));
    assert!(
        none.linked.is_empty(),
        "a file whose commands name no library yields none: {:?}",
        none.linked
    );
    assert!(
        mach_shape(b"\x7fELF\x02\x01\x01").is_err(),
        "and something that is not a Mach-O file at all is refused"
    );
    let mut truncated = file;
    truncated.truncate(truncated.len().saturating_sub(SEGMENT_LENGTH));
    assert!(
        mach_shape(&truncated).is_err(),
        "as is a file whose commands run past its end"
    );
}
