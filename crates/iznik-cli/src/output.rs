//! Where the binary writes what it found: one hand-built JSON object per
//! line. The one thing that does not come through here is a usage line, which
//! is an answer to a question rather than an account of a host.
//!
//! By hand, because what these commands print is a handful of shapes and a
//! writer for them is smaller than the reasoning about which crate should
//! write them — and because the one rule that matters here is that every line
//! is one object, which a writer that knows only objects cannot break. What
//! reads the lines back is a real parser: the cases hold this to `serde_json`
//! rather than to itself.

use core::fmt::Write as _;
use std::io::{self, Write};

/// How many bits of a byte one character of base 64 carries.
const BASE64_BITS: u32 = 6;

/// How many characters base 64 has, which is what its name says.
const BASE64_CHARACTERS: usize = 64;

/// The characters base 64 is written with, in their own order.
const BASE64_ALPHABET: &[u8; BASE64_CHARACTERS] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// How many bytes go into one group of four characters.
const BASE64_GROUP: usize = 3;

/// The bits one character of base 64 takes from what it is given.
const BASE64_MASK: u32 = 0x3F;

/// How much of what a value takes to write, the writer asks for at the start:
/// twice its length, which for base 64 is nearer the mark than its length.
const ROOM: usize = 2;

/// The first character that needs no escaping of its own.
const FIRST_PLAIN: char = ' ';

/// A value one of these commands prints.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Nothing, which is what a field that has no answer says.
    Null,
    /// True or false.
    Truth(bool),
    /// A whole number: a count, a sequence, a version.
    Whole(u64),
    /// A number with a fraction: a duration in milliseconds, a percentile.
    Fraction(f64),
    /// Text, escaped as JSON asks.
    Text(String),
    /// A list of values.
    List(Vec<Value>),
    /// An object, in the order its fields were given — what a person reads
    /// twice is easier to read when the fields have not moved.
    Object(Vec<(String, Value)>),
}

/// Text, as a value.
#[must_use]
pub fn text(said: &str) -> Value {
    Value::Text(said.to_owned())
}

/// An object, from fields named in order.
#[must_use]
pub fn object(fields: Vec<(&str, Value)>) -> Value {
    Value::Object(
        fields
            .into_iter()
            .map(|(named, held)| (named.to_owned(), held))
            .collect(),
    )
}

/// Bytes, as base 64 text.
///
/// A pane's output is bytes and not text — half a character may arrive before
/// the other half — so it crosses as base 64, which is exact, rather than as
/// text that would have to be mended to be printed.
#[must_use]
pub fn bytes(held: &[u8]) -> Value {
    let mut written = String::with_capacity(held.len().saturating_mul(ROOM));
    for group in held.chunks(BASE64_GROUP) {
        let mut carried: u32 = 0;
        for at in 0..BASE64_GROUP {
            let byte = group.get(at).copied().unwrap_or(0);
            carried = (carried << u8::BITS) | u32::from(byte);
        }
        for at in 0..=BASE64_GROUP {
            let shift = BASE64_BITS
                .saturating_mul(u32::try_from(BASE64_GROUP.saturating_sub(at)).unwrap_or_default());
            if at > group.len() {
                written.push('=');
                continue;
            }
            let index = usize::try_from((carried >> shift) & BASE64_MASK).unwrap_or_default();
            written.push(char::from(
                BASE64_ALPHABET.get(index).copied().unwrap_or(b'A'),
            ));
        }
    }
    Value::Text(written)
}

/// Writes one value as a line of its own.
///
/// # Errors
///
/// Whatever the writer says, which for a closed pipe is what ends a `tail`.
pub fn line(writer: &mut impl Write, held: &Value) -> io::Result<()> {
    written(writer, held)?;
    writeln!(writer)?;
    writer.flush()
}

/// Writes one refusal, which is one object with the layer and the message.
///
/// # Errors
///
/// Whatever the writer says.
pub fn refusal(writer: &mut impl Write, layer: &str, message: &str) -> io::Result<()> {
    line(
        writer,
        &object(vec![("error", text(message)), ("layer", text(layer))]),
    )
}

/// Writes one value, without a line of its own.
///
/// # Errors
///
/// Whatever the writer says.
fn written(writer: &mut impl Write, held: &Value) -> io::Result<()> {
    match held {
        Value::Null => write!(writer, "null"),
        Value::Truth(truth) => write!(writer, "{truth}"),
        Value::Whole(number) => write!(writer, "{number}"),
        // JSON has no word for what a division by nothing makes, so a number
        // that is not one is nothing.
        Value::Fraction(number) if !number.is_finite() => write!(writer, "null"),
        Value::Fraction(number) => write!(writer, "{number}"),
        Value::Text(said) => write!(writer, "\"{}\"", escaped(said)),
        Value::List(held) => {
            write!(writer, "[")?;
            for (at, value) in held.iter().enumerate() {
                if at > 0 {
                    write!(writer, ",")?;
                }
                written(writer, value)?;
            }
            write!(writer, "]")
        }
        Value::Object(fields) => {
            write!(writer, "{{")?;
            for (at, (named, value)) in fields.iter().enumerate() {
                if at > 0 {
                    write!(writer, ",")?;
                }
                write!(writer, "\"{}\":", escaped(named))?;
                written(writer, value)?;
            }
            write!(writer, "}}")
        }
    }
}

/// Text with what JSON will not take written the way JSON takes it.
fn escaped(said: &str) -> String {
    let mut written = String::with_capacity(said.len());
    for letter in said.chars() {
        match letter {
            '"' => written.push_str("\\\""),
            '\\' => written.push_str("\\\\"),
            '\n' => written.push_str("\\n"),
            '\r' => written.push_str("\\r"),
            '\t' => written.push_str("\\t"),
            // Everything below a space has no spelling of its own, and
            // everything above it is text a reader takes as it comes.
            control if control < FIRST_PLAIN => {
                let _wrote = write!(written, "\\u{:04x}", u32::from(control));
            }
            plain => written.push(plain),
        }
    }
    written
}
