//! Linux landlock enforcement. Landlock restricts the calling process, so the sandboxed
//! command runs via a helper that restricts itself then `exec`s the real argv
//! (`Command::exec` is safe; `pre_exec` would need `unsafe`, denied workspace-wide).

use landlock::{
    ABI, Access, AccessFs, AccessNet, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset,
    RulesetAttr, RulesetCreatedAttr, RulesetStatus,
};

use crate::{error::SandboxError, policy::Policy};

const ABI_TARGET: ABI = ABI::V4;

/// True iff the running kernel supports landlock at all (ABI v1).
pub fn available() -> bool {
    Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(ABI::V1))
        .and_then(|r| r.create())
        .is_ok()
}

/// Restrict the *current process* (irreversible). Reads/exec allowed everywhere; writes only
/// under `policy.write_roots`; TCP bind/connect denied unless `policy.network`.
pub fn restrict_self(policy: &Policy) -> Result<(), SandboxError> {
    let setup = |e: &dyn std::fmt::Display| SandboxError::Setup(e.to_string());
    let mut rs = Ruleset::default()
        .handle_access(AccessFs::from_all(ABI_TARGET))
        .map_err(|e| setup(&e))?;
    if !policy.network {
        rs = rs
            .handle_access(AccessNet::from_all(ABI_TARGET))
            .map_err(|e| setup(&e))?;
    }
    let mut rs = rs.create().map_err(|e| setup(&e))?;
    rs = rs
        .add_rule(PathBeneath::new(
            PathFd::new("/").map_err(|e| setup(&e))?,
            AccessFs::from_read(ABI_TARGET),
        ))
        .map_err(|e| setup(&e))?;
    for root in policy.existing_roots() {
        let fd = PathFd::new(&root).map_err(|e| setup(&e))?;
        rs = rs
            .add_rule(PathBeneath::new(fd, AccessFs::from_all(ABI_TARGET)))
            .map_err(|e| setup(&e))?;
    }
    let status = rs.restrict_self().map_err(|e| setup(&e))?;
    if status.ruleset == RulesetStatus::NotEnforced && policy.strict {
        return Err(SandboxError::Unavailable(
            "landlock not enforced by kernel".into(),
        ));
    }
    Ok(())
}
