//! Kernel-level check: sandboxed shell cannot write outside root. Skips if landlock unavailable.
#![cfg(target_os = "linux")]
#![allow(clippy::print_stderr)]

use hk_sandbox::{BackendKind, Policy, wrap_command_with_helper};
use std::path::Path;

fn run(policy: &Policy, script: &str) -> std::process::ExitStatus {
    let helper = Path::new(env!("CARGO_BIN_EXE_hk-sandbox-exec"));
    let argv: Vec<String> = ["sh", "-c", script].iter().map(|s| s.to_string()).collect();
    wrap_command_with_helper(policy, &argv, helper)
        .unwrap()
        .into_command()
        .status()
        .unwrap()
}

#[test]
fn write_outside_root_denied_inside_allowed() {
    if !hk_sandbox::landlock_available() {
        eprintln!("skip: landlock unsupported by kernel");
        return;
    }
    let root = tempfile::tempdir_in("/var/tmp")
        .or_else(|_| tempfile::tempdir())
        .unwrap();
    let outside = tempfile::tempdir_in("/var/tmp")
        .or_else(|_| tempfile::tempdir())
        .unwrap();
    let mut p = Policy::for_workspace(root.path());
    // Drop the temp-dir roots so `outside` (possibly under /tmp) is really outside.
    p.write_roots
        .retain(|r| r == root.path() || r.starts_with("/dev"));
    p.backend = BackendKind::Landlock;
    p.env_allowlist.push("PATH".into());

    let ok = root.path().join("in.txt");
    let bad = outside.path().join("out.txt");
    assert!(run(&p, &format!("echo x > '{}'", ok.display())).success());
    assert!(ok.exists());
    assert!(!run(&p, &format!("echo x > '{}'", bad.display())).success());
    assert!(!bad.exists());
}
