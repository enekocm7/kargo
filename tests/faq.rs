//! The links `help:` lines print (`kargo_util::help::FAQ`) must point at headings of `reference/faq.md`.

use kargo_util::help::FAQ;

/// The anchor Starlight (github-slugger) gives a heading: lowercase, punctuation removed, spaces to hyphens.
fn slug(heading: &str) -> String {
    heading
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

#[test]
fn every_faq_link_points_at_a_heading() {
    let page = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/website/src/content/docs/reference/faq.md")).unwrap();
    let anchors: Vec<String> = page.lines().filter_map(|l| l.strip_prefix("### ")).map(slug).collect();
    for (message, anchor) in FAQ {
        assert!(anchors.iter().any(|a| a == anchor), "the FAQ has no heading with the anchor `{anchor}` (for `{message}`)");
    }
}
