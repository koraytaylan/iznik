//! A fixture: the literal positions the check must and must not report.
pub const LIMIT: usize = 4096;
pub static COUNT: u64 = 7;
pub enum Kind {
    Alpha = 3,
    Beta = 4,
}
pub fn values() -> usize {
    let zero = 0;
    let one = 1;
    let float_one = 1.0;
    let float_zero = 0.0;
    let two = 2;
    let array = [0; 4];
    let typed: [u8; 3] = [0; 1];
    let inside_macro = vec![0; 5];
    let half = 2.5;
    zero + one + two + array.len() + typed.len() + inside_macro.len()
}
pub fn matched(count: usize) -> bool {
    match count {
        2 => true,
        _ => false,
    }
}
