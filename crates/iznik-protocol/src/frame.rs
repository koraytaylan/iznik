//! The frame codec: a little-endian payload length, a channel byte and the payload, with a decoder that resumes across any read boundary.
//!
//! Filled by task `frame-codec` of plan 0001; until then this module holds only its documentation.
