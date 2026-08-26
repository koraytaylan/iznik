//! One accepted stream: the handshake, the dispatch of every control message, and the single writer that owns the order of frames on the wire.
//!
//! Filled by task `client-connections` of plan 0004; until then this module holds only its documentation.
