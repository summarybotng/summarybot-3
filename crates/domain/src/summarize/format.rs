//! Summary rendering (PRD §4; FMT-001/005).
//!
//! Pure: turns an [`ExtractedSummary`] into a display string. Markdown and plain
//! text are platform-agnostic and built here; the JSON format and per-platform
//! *rich* envelopes (Discord embeds, Slack blocks) are host concerns layered on
//! top (they need serde / platform SDKs). Empty sections are omitted so a terse
//! summary stays terse.

use super::extract::{ExtractedSummary, ReferencedClaim};

/// Compact source suffix for a claim (ADR-004): "(sources: #1 Alice, #4 Bob)"
/// using each reference's 1-based position + author. Empty when ungrounded.
fn refs_suffix(claim: &ReferencedClaim) -> String {
    if claim.references.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = claim
        .references
        .iter()
        .map(|r| format!("#{} {}", r.position, r.author_name))
        .collect();
    format!(" (sources: {})", parts.join(", "))
}

/// A platform-agnostic output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryFormat {
    /// GitHub-flavored markdown (headings, bullet lists, checkboxes).
    Markdown,
    /// Plain text (no markup) for platforms/contexts that don't render markdown.
    Plain,
    /// HTML (escaped) for email bodies and Confluence-style pages.
    Html,
}

/// Render `summary` in `format`.
pub fn render(summary: &ExtractedSummary, format: SummaryFormat) -> String {
    match format {
        SummaryFormat::Markdown => render_markdown(summary),
        SummaryFormat::Plain => render_plain(summary),
        SummaryFormat::Html => render_html(summary),
    }
}

/// Escape text for safe inclusion in HTML (also valid for Confluence storage).
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_html(s: &ExtractedSummary) -> String {
    let mut out = String::new();
    if !s.text.trim().is_empty() {
        out.push_str(&format!("<p>{}</p>\n", esc(s.text.trim())));
    }
    let section = |out: &mut String, title: &str, items: &[String]| {
        if !items.is_empty() {
            out.push_str(&format!("<h2>{title}</h2>\n<ul>\n"));
            for i in items {
                out.push_str(&format!("<li>{}</li>\n", esc(i)));
            }
            out.push_str("</ul>\n");
        }
    };
    if !s.key_points.is_empty() {
        out.push_str("<h2>Key Points</h2>\n<ul>\n");
        for k in &s.key_points {
            let suffix = refs_suffix(k);
            if suffix.is_empty() {
                out.push_str(&format!("<li>{}</li>\n", esc(&k.text)));
            } else {
                out.push_str(&format!(
                    "<li>{} <span class=\"sources\">{}</span></li>\n",
                    esc(&k.text),
                    esc(&suffix)
                ));
            }
        }
        out.push_str("</ul>\n");
    }
    if !s.action_items.is_empty() {
        out.push_str("<h2>Action Items</h2>\n<ul>\n");
        for a in &s.action_items {
            match &a.assignee {
                Some(who) => out.push_str(&format!("<li>{} — {}</li>\n", esc(&a.text), esc(who))),
                None => out.push_str(&format!("<li>{}</li>\n", esc(&a.text))),
            }
        }
        out.push_str("</ul>\n");
    }
    if !s.participants.is_empty() {
        out.push_str(&format!(
            "<h2>Participants</h2>\n<p>{}</p>\n",
            esc(&s.participants.join(", "))
        ));
    }
    section(&mut out, "Technical Terms", &s.technical_terms);
    out.trim_end().to_string()
}

fn render_markdown(s: &ExtractedSummary) -> String {
    let mut out = String::new();
    if !s.text.trim().is_empty() {
        out.push_str(s.text.trim());
        out.push_str("\n\n");
    }
    if !s.key_points.is_empty() {
        out.push_str("## Key Points\n");
        for p in &s.key_points {
            out.push_str(&format!("- {}{}\n", p.text, refs_suffix(p)));
        }
        out.push('\n');
    }
    if !s.action_items.is_empty() {
        out.push_str("## Action Items\n");
        for a in &s.action_items {
            match &a.assignee {
                Some(who) => out.push_str(&format!("- [ ] {} — {who}\n", a.text)),
                None => out.push_str(&format!("- [ ] {}\n", a.text)),
            }
        }
        out.push('\n');
    }
    if !s.participants.is_empty() {
        out.push_str(&format!(
            "## Participants\n{}\n\n",
            s.participants.join(", ")
        ));
    }
    if !s.technical_terms.is_empty() {
        out.push_str(&format!(
            "## Technical Terms\n{}\n\n",
            s.technical_terms.join(", ")
        ));
    }
    if !s.citations.is_empty() {
        out.push_str("## Sources\n");
        for c in &s.citations {
            match &c.quote {
                Some(q) => out.push_str(&format!("- `{}`: \"{q}\"\n", c.message_id.as_str())),
                None => out.push_str(&format!("- `{}`\n", c.message_id.as_str())),
            }
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

fn render_plain(s: &ExtractedSummary) -> String {
    let mut out = String::new();
    if !s.text.trim().is_empty() {
        out.push_str(s.text.trim());
        out.push_str("\n\n");
    }
    if !s.key_points.is_empty() {
        out.push_str("Key Points:\n");
        for p in &s.key_points {
            out.push_str(&format!("- {}{}\n", p.text, refs_suffix(p)));
        }
        out.push('\n');
    }
    if !s.action_items.is_empty() {
        out.push_str("Action Items:\n");
        for a in &s.action_items {
            match &a.assignee {
                Some(who) => out.push_str(&format!("- {} ({who})\n", a.text)),
                None => out.push_str(&format!("- {}\n", a.text)),
            }
        }
        out.push('\n');
    }
    if !s.participants.is_empty() {
        out.push_str(&format!("Participants: {}\n", s.participants.join(", ")));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::super::extract::{ActionItem, ResolvedCitation};
    use super::*;
    use crate::MessageId;

    fn summary() -> ExtractedSummary {
        ExtractedSummary {
            text: "The team agreed to launch Friday.".into(),
            key_points: vec!["Launch on Friday".into(), "Changelog needed".into()],
            action_items: vec![
                ActionItem {
                    text: "write changelog".into(),
                    assignee: Some("Alice".into()),
                },
                ActionItem {
                    text: "notify users".into(),
                    assignee: None,
                },
            ],
            technical_terms: vec!["WASM".into()],
            participants: vec!["Alice".into(), "Bob".into()],
            citations: vec![ResolvedCitation {
                message_id: MessageId::parse("m0").unwrap(),
                quote: Some("ship it".into()),
            }],
        }
    }

    #[test]
    fn markdown_has_sections_and_checkboxes() {
        let md = render(&summary(), SummaryFormat::Markdown);
        assert!(md.contains("## Key Points"));
        assert!(md.contains("- Launch on Friday"));
        assert!(md.contains("- [ ] write changelog — Alice"));
        assert!(md.contains("- [ ] notify users"));
        assert!(md.contains("## Participants\nAlice, Bob"));
        assert!(md.contains("## Sources"));
        assert!(md.contains("`m0`: \"ship it\""));
    }

    #[test]
    fn plain_has_no_markdown_markup() {
        let txt = render(&summary(), SummaryFormat::Plain);
        assert!(!txt.contains('#'));
        assert!(!txt.contains("[ ]"));
        assert!(txt.contains("Key Points:"));
        assert!(txt.contains("Participants: Alice, Bob"));
    }

    #[test]
    fn html_is_escaped_and_structured() {
        let mut s = summary();
        s.text = "ship <b>now</b> & soon".into();
        let html = render(&s, SummaryFormat::Html);
        assert!(html.contains("<p>ship &lt;b&gt;now&lt;/b&gt; &amp; soon</p>"));
        assert!(html.contains("<h2>Key Points</h2>"));
        assert!(html.contains("<li>Launch on Friday</li>"));
        assert!(html.contains("<li>write changelog — Alice</li>"));
        // No raw markdown markup.
        assert!(!html.contains("## "));
        assert!(!html.contains("- ["));
    }

    #[test]
    fn empty_sections_are_omitted() {
        let bare = ExtractedSummary {
            text: "Just a sentence.".into(),
            key_points: vec![],
            action_items: vec![],
            technical_terms: vec![],
            participants: vec![],
            citations: vec![],
        };
        let md = render(&bare, SummaryFormat::Markdown);
        assert_eq!(md, "Just a sentence.");
        assert!(!md.contains("##"));
    }
}
