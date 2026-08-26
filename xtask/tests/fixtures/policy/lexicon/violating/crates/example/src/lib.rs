//! A fixture: every form of declaration the vocabulary check must split.
pub struct Buffer {
    pub count: usize,
}
pub fn read_buf(buf: &[u8]) -> usize {
    buf.len()
}
pub fn snake_case_name() {}
pub struct CamelCaseName;
pub const SCREAMING_CASE: usize = 1;
pub fn utf8_value(x86_64: usize) -> usize {
    let _unused = 1;
    x86_64
}
pub fn r#type() {}
pub fn uses_std(text: String) -> String {
    text.to_uppercase()
}
pub fn closure_words() {
    let alpha = |beta: usize| beta;
    alpha(1);
}
pub struct Generic<'lifetime, Item> {
    pub item: &'lifetime Item,
}
pub enum Kind {
    First,
    Second,
}
pub fn labelled() {
    'outer: loop {
        break 'outer;
    }
}
pub use std::fmt::Display as Shown;
pub fn matched(kind: Option<Kind>) -> bool {
    match kind {
        None => false,
        Some(Kind::First) => true,
        Some(found) => matches!(found, Kind::Second),
    }
}
