//! Agent modes (opencode): which tools the model sees and which calls need a human.
// port: opencode agent modes (`build` all tools, `plan` read-only fs + bash asks)

use hk_tools::Risk;

/// Built-in mode. Chosen per agent in [`crate::AgentConfig`]; swapped between turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    /// All tools; only `Risk >= Ask` calls need approval.
    #[default]
    Build,
    /// Read-only filesystem: file writers are hidden and refused, `Ask`-risk tools (bash,
    /// web fetch) still go through approval.
    Plan,
}

/// What the policy does with one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask,
    /// Refused without asking; the text becomes the error tool result.
    Deny(String),
}

/// Tools that mutate files. Name-based because `Risk` says "scrutinise", not "writes".
const FS_WRITERS: &[&str] = &["write", "edit"];

impl Mode {
    /// Stable lowercase name (config / UI).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Plan => "plan",
        }
    }

    /// Whether the tool schema is offered to the model at all in this mode.
    pub fn exposes(self, tool: &str) -> bool {
        !(self == Self::Plan && FS_WRITERS.contains(&tool))
    }

    /// Policy for one call. Enforced even for tools hidden from the schema, since models
    /// can still emit calls they were not offered.
    pub fn decide(self, tool: &str, risk: Risk) -> Decision {
        if !self.exposes(tool) {
            return Decision::Deny(format!(
                "plan mode is read-only: `{tool}` is not available; describe the change in your plan instead"
            ));
        }
        if risk >= Risk::Ask {
            Decision::Ask
        } else {
            Decision::Allow
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_denies_writers_asks_bash_allows_safe() {
        assert!(matches!(
            Mode::Plan.decide("write", Risk::Ask),
            Decision::Deny(_)
        ));
        assert!(matches!(
            Mode::Plan.decide("edit", Risk::Ask),
            Decision::Deny(_)
        ));
        assert_eq!(Mode::Plan.decide("bash", Risk::Ask), Decision::Ask);
        assert_eq!(Mode::Plan.decide("read", Risk::Safe), Decision::Allow);
        assert!(!Mode::Plan.exposes("edit") && Mode::Plan.exposes("bash"));
    }

    #[test]
    fn build_asks_only_for_ask_risk() {
        assert_eq!(Mode::Build.decide("write", Risk::Ask), Decision::Ask);
        assert_eq!(Mode::Build.decide("rm", Risk::Dangerous), Decision::Ask);
        assert_eq!(Mode::Build.decide("grep", Risk::Safe), Decision::Allow);
        assert!(Mode::Build.exposes("write"));
    }
}
