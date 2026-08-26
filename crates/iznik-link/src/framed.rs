//! Frames over any duplex byte stream: one vectored write per frame out, a resuming decoder in, split halves, and the parts a compression layer is built from.
//!
//! Filled by task `framed-link` of plan 0001; until then this module holds only its documentation.
