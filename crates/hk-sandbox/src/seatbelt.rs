//! macOS seatbelt profile generation. Pure string building, so testable on any OS.

use crate::policy::Policy;

pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Allow-by-default, then deny writes and (optionally) network, re-allowing write roots.
/// Later rules win in SBPL, so allows come after the broad deny.
pub fn profile(policy: &Policy) -> String {
    let mut p = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
    let roots = policy.existing_roots();
    if !roots.is_empty() {
        p.push_str("(allow file-write*\n");
        for r in &roots {
            let form = if r.is_dir() { "subpath" } else { "literal" };
            p.push_str(&format!("  ({form} {})\n", quote(&r.to_string_lossy())));
        }
        p.push_str(")\n");
    }
    if !policy.network {
        p.push_str("(deny network*)\n");
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_denies_network_and_scopes_writes() {
        let d = tempfile::tempdir().unwrap();
        let pol = Policy::for_workspace(d.path());
        let s = profile(&pol);
        assert!(s.contains("(deny file-write*)"));
        assert!(s.contains("(deny network*)"));
        let root = d.path().canonicalize().unwrap();
        assert!(s.contains(&format!("(subpath {})", quote(&root.to_string_lossy()))));
    }

    #[test]
    fn network_on_omits_deny() {
        let d = tempfile::tempdir().unwrap();
        let mut pol = Policy::for_workspace(d.path());
        pol.network = true;
        assert!(!profile(&pol).contains("(deny network*)"));
    }

    #[test]
    fn quote_escapes() {
        assert_eq!(quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }
}
