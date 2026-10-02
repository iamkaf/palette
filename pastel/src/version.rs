//! Pack version ordering.

use std::cmp::Ordering;

/// Compares dotted versions such as `1.0.1` and `1.1.0`. Numeric segments
/// compare as numbers, anything else as text, and `-suffix` parts are ignored.
pub fn compare(a: &str, b: &str) -> Ordering {
    let a = a.trim().trim_start_matches('v');
    let b = b.trim().trim_start_matches('v');
    if a == b {
        return Ordering::Equal;
    }
    let a: Vec<&str> = a.split('.').collect();
    let b: Vec<&str> = b.split('.').collect();
    for index in 0..a.len().max(b.len()) {
        let left = segment(&a, index);
        let right = segment(&b, index);
        let order = match (left.parse::<i64>(), right.parse::<i64>()) {
            (Ok(left), Ok(right)) => left.cmp(&right),
            _ => left.cmp(right),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    Ordering::Equal
}

fn segment<'a>(parts: &[&'a str], index: usize) -> &'a str {
    let part = parts.get(index).copied().unwrap_or("");
    part.split('-').next().unwrap_or(part)
}

/// A short label such as `FOREVER WORLD v1.1.0  ·  Minecraft 26.2`.
pub fn pack_line(name: &str, version: &str, minecraft: &str) -> String {
    let mut line = format!("{name} v{version}");
    if !minecraft.is_empty() {
        line.push_str(&format!("  ·  Minecraft {minecraft}"));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_dotted_versions_numerically() {
        assert_eq!(compare("1.0.1", "1.1.0"), Ordering::Less);
        assert_eq!(compare("1.1.0", "1.0.1"), Ordering::Greater);
        assert_eq!(compare("1.1.0", "v1.1.0"), Ordering::Equal);
        assert_eq!(compare("2.0.0", "1.9.9"), Ordering::Greater);
        assert_eq!(compare("1.0.10", "1.0.9"), Ordering::Greater);
    }
}
