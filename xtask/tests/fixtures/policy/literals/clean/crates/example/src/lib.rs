//! A fixture with no magic number.
pub const LIMIT: usize = 4096;
pub fn values() -> usize {
    let zero = 0;
    let one = 1;
    let float = 1.0;
    let array = [0; 1];
    zero + one + array.len() + LIMIT
}
