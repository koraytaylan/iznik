//! A fixture without waivers.
#[cfg(unix)]
#[cfg_attr(unix, derive(Clone))]
#[derive(Debug)]
#[must_use]
pub struct Fine;
