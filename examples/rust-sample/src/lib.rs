//! Tiny sample library for exercising the CSP Rust adapter. `add` is covered by a
//! test; `unused` is deliberately left uncovered so the gutter shows a gap.

pub fn add(a: i64, b: i64) -> i64 {
    a + b
}

pub fn unused(a: i64, b: i64) -> i64 {
    a - b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds() {
        assert_eq!(add(2, 3), 5);
    }
}
