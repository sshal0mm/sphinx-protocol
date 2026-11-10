//! The catalog is public API: codes are well-formed, unique, and the
//! committed `docs/ERRORS.md` matches the generator output.

use sphinx_protocol::catalog::{render_error_docs, Code};
use std::collections::HashSet;
use std::path::Path;

#[test]
fn codes_are_well_formed_and_unique() {
    let mut seen = HashSet::new();
    for code in Code::ALL {
        let s = code.as_str();
        assert!(s.starts_with("SPHINX_E_"), "{s}");
        let rest = &s["SPHINX_E_".len()..];
        assert!(rest.contains('_'), "{s} needs a domain and a reason");
        assert!(
            rest.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'),
            "{s}"
        );
        assert!(!rest.starts_with('_') && !rest.ends_with('_') && !rest.contains("__"), "{s}");
        assert!(seen.insert(s), "duplicate code {s}");
        assert_eq!(s.parse::<Code>(), Ok(*code));
        assert!(!code.hint().is_empty() && !code.hint().contains('\n'), "{s}");
    }
}

#[test]
fn error_docs_are_up_to_date() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/ERRORS.md");
    let expected = render_error_docs();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &expected).unwrap();
    }
    let actual = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        actual == expected,
        "docs/ERRORS.md is stale; run `cargo run -p sphinx-protocol --bin gen-error-docs > docs/ERRORS.md`"
    );
}
