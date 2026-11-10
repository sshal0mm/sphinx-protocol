//! Job titles. Every job carries a short title, so it is listed by name
//! rather than by number. A task description written as Markdown names its
//! job in its first `# ` heading; [`from_spec`] reads it from there.

/// Longest title the escrow accepts, in characters.
pub const MAX_TITLE_CHARS: usize = 80;

/// Cleans a title: one line, single spaces, at most [`MAX_TITLE_CHARS`]
/// characters. `None` if nothing is left.
#[must_use]
pub fn clean(title: &str) -> Option<String> {
    let joined = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = joined.chars().take(MAX_TITLE_CHARS).collect();
    let cut = cut.trim().to_owned();
    (!cut.is_empty()).then_some(cut)
}

/// The title a task description gives itself: the text of its first `# `
/// heading, cleaned.
#[must_use]
pub fn from_spec(spec: &str) -> Option<String> {
    spec.lines().map(str::trim_start).find_map(|l| l.strip_prefix("# ")).and_then(clean)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_heading_names_the_job() {
        assert_eq!(
            from_spec("intro\n# Liquidation price  for a perp\n\n## Examples").as_deref(),
            Some("Liquidation price for a perp")
        );
        assert_eq!(from_spec("no heading here"), None);
        assert_eq!(clean(&"a".repeat(200)).map(|t| t.chars().count()), Some(MAX_TITLE_CHARS));
        assert_eq!(clean("   "), None);
    }
}
