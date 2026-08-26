//! Frames over a duplex byte stream and streaming compression, written once for both ends and the test client.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod compression;
pub mod framed;
