//! A fixture: every attribute the check reports.
#![allow(dead_code)]
#[allow(clippy::all)]
pub fn allowed() {}
#[expect(unused)]
pub fn expected() {}
#[cfg(test)]
mod tests {}
#[cfg(all(test, unix))]
mod more {}
#[cfg(unix)]
pub fn unix_only() {}
#[cfg_attr(test, allow(dead_code))]
pub fn conditional() {}
