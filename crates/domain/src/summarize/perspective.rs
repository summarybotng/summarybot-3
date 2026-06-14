//! Built-in summary perspectives (ADR-133) — named instruction presets that
//! steer the summary's lens (audience/voice). A perspective resolves to a block
//! of prompt instructions; the host prepends it to any per-workspace guidance.
//!
//! These mirror v2's built-ins (General/Developer/Marketing/Executive/Support);
//! tenant-defined named templates (the `prompt_templates` store) layer on top.

/// One of the built-in summary lenses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Perspective {
    /// Balanced, audience-neutral (the default).
    General,
    /// Engineering lens: decisions, APIs, blockers, technical detail.
    Developer,
    /// Marketing lens: launches, messaging, audience/sentiment, wins.
    Marketing,
    /// Executive lens: outcomes, risks, decisions needed — terse.
    Executive,
    /// Support lens: issues raised, resolutions, outstanding problems.
    Support,
}

impl Perspective {
    /// Stable wire id.
    pub fn as_str(self) -> &'static str {
        match self {
            Perspective::General => "general",
            Perspective::Developer => "developer",
            Perspective::Marketing => "marketing",
            Perspective::Executive => "executive",
            Perspective::Support => "support",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "general" => Some(Perspective::General),
            "developer" => Some(Perspective::Developer),
            "marketing" => Some(Perspective::Marketing),
            "executive" => Some(Perspective::Executive),
            "support" => Some(Perspective::Support),
            _ => None,
        }
    }

    /// Human label for the UI.
    pub fn label(self) -> &'static str {
        match self {
            Perspective::General => "General",
            Perspective::Developer => "Developer",
            Perspective::Marketing => "Marketing",
            Perspective::Executive => "Executive",
            Perspective::Support => "Support",
        }
    }

    /// The instruction block this perspective contributes, or `None` for
    /// [`Perspective::General`] (no extra steering — the neutral default).
    pub fn instructions(self) -> Option<&'static str> {
        match self {
            Perspective::General => None,
            Perspective::Developer => Some(
                "Summarize for a software engineering audience. Emphasize technical \
                 decisions, API/design changes, bugs, and blockers. Keep code/library \
                 names precise.",
            ),
            Perspective::Marketing => Some(
                "Summarize for a marketing audience. Emphasize launches, messaging, \
                 audience sentiment, partnerships, and notable wins. Keep it engaging.",
            ),
            Perspective::Executive => Some(
                "Summarize for an executive audience. Lead with outcomes, risks, and \
                 decisions needed. Be terse — no low-level detail.",
            ),
            Perspective::Support => Some(
                "Summarize for a support/operations audience. Emphasize issues raised, \
                 their resolutions, and outstanding/unresolved problems.",
            ),
        }
    }

    /// All built-ins, in display order.
    pub fn all() -> [Perspective; 5] {
        [
            Perspective::General,
            Perspective::Developer,
            Perspective::Marketing,
            Perspective::Executive,
            Perspective::Support,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_general_has_no_instructions() {
        for p in Perspective::all() {
            assert_eq!(Perspective::parse(p.as_str()), Some(p));
        }
        assert!(Perspective::General.instructions().is_none());
        assert!(Perspective::Developer.instructions().is_some());
    }
}
