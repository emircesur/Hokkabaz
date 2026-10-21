//! hk-sandbox: policy model + backends that confine spawned commands (bash) and gate file writes.
//!
//! port: dsh docs/capability-seams.md (fs/subprocess/shell/sandbox share one execution world)
//!
//! Backends: landlock (Linux, not Android), seatbelt (macOS), none + allowlists (rest).
//! Entry points: [`wrap_command`] for subprocesses, [`Policy::check_write`] for in-process file tools.
//!
//! Landlock needs the child to restrict itself before exec. Hosts that use landlock must call
//! [`maybe_run_helper`] first thing in `main` (or pass the `hk-sandbox-exec` binary to
//! [`wrap_command_with_helper`]).

mod error;
#[cfg(target_os = "linux")]
mod landlock_backend;
mod policy;
mod seatbelt;

use std::ffi::OsString;
use std::path::Path;

pub use error::SandboxError;
pub use policy::{BackendKind, Policy, default_env_allowlist};

/// Argv marker that turns a process into the landlock helper.
pub const HELPER_ARG: &str = "--hk-sandbox-exec";

/// True iff landlock can be enforced on this host. Always false off Linux (incl. Android).
pub fn landlock_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        landlock_backend::available()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Ready-to-spawn command: program, args and a fully-specified (cleared + allowlisted) env.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    pub backend: BackendKind,
}

impl WrappedCommand {
    pub fn into_command(self) -> std::process::Command {
        let mut c = std::process::Command::new(self.program);
        c.args(self.args).env_clear().envs(self.env);
        c
    }
}

/// Wrap `argv` per `policy`. Landlock re-execs the current binary as helper; see crate docs.
pub fn wrap_command(policy: &Policy, argv: &[String]) -> Result<WrappedCommand, SandboxError> {
    let helper = std::env::current_exe()?;
    wrap_command_with_helper(policy, argv, &helper)
}

/// Like [`wrap_command`] with an explicit landlock helper executable.
pub fn wrap_command_with_helper(
    policy: &Policy,
    argv: &[String],
    helper: &Path,
) -> Result<WrappedCommand, SandboxError> {
    let first = argv.first().ok_or(SandboxError::EmptyArgv)?;
    policy.check_command(first)?;
    let backend = policy.backend.resolve();
    let env = filtered_env(policy);
    let os = |s: &String| OsString::from(s);

    let (program, args) = match backend {
        BackendKind::Landlock => {
            if !cfg!(target_os = "linux") {
                return Err(SandboxError::Unavailable("landlock requires linux".into()));
            }
            let mut pol = policy.clone();
            pol.backend = BackendKind::Landlock;
            let mut a = vec![
                OsString::from(HELPER_ARG),
                OsString::from(serde_json::to_string(&pol)?),
                OsString::from("--"),
            ];
            a.extend(argv.iter().map(os));
            (helper.as_os_str().to_owned(), a)
        }
        BackendKind::Seatbelt => {
            let mut a = vec![
                OsString::from("-p"),
                OsString::from(seatbelt::profile(policy)),
                OsString::from("--"),
            ];
            a.extend(argv.iter().map(os));
            (OsString::from(seatbelt::SANDBOX_EXEC), a)
        }
        BackendKind::None | BackendKind::Auto => {
            tracing::warn!("sandbox: no kernel enforcement; env filter + allowlists only");
            (os(first), argv[1..].iter().map(os).collect())
        }
    };
    Ok(WrappedCommand {
        program,
        args,
        env,
        backend,
    })
}

fn filtered_env(policy: &Policy) -> Vec<(OsString, OsString)> {
    std::env::vars_os()
        .filter(|(k, _)| {
            policy
                .env_allowlist
                .iter()
                .any(|a| k.to_str() == Some(a.as_str()))
        })
        .collect()
}

/// If this process was started as the landlock helper, restrict self and `exec` the target
/// (never returns). Otherwise returns immediately. Call first thing in `main`.
#[allow(clippy::print_stderr)] // helper runs pre-exec in a fresh process; stderr is the only channel
pub fn maybe_run_helper() {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(HELPER_ARG)) {
        return;
    }
    let fail = |msg: String| -> ! {
        eprintln!("hk-sandbox: {msg}");
        std::process::exit(126)
    };
    let Some(json) = args.next() else {
        fail("missing policy".into())
    };
    let policy: Policy = match json.to_str().map(serde_json::from_str) {
        Some(Ok(p)) => p,
        _ => fail("bad policy json".into()),
    };
    let _ = args.next(); // "--"
    let Some(program) = args.next() else {
        fail("missing command".into())
    };
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        if let Err(e) = landlock_backend::restrict_self(&policy) {
            fail(e.to_string());
        }
        let err = std::process::Command::new(program).args(args).exec();
        fail(format!("exec failed: {err}"));
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (policy, program);
        fail("landlock helper unsupported on this platform".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn none_backend_passes_argv_and_filters_env() {
        let d = tempfile::tempdir().unwrap();
        let mut p = Policy::for_workspace(d.path());
        p.backend = BackendKind::None;
        p.env_allowlist = vec!["PATH".into()];
        let w = wrap_command(&p, &argv(&["echo", "hi"])).unwrap();
        assert_eq!(w.program, OsString::from("echo"));
        assert_eq!(w.args, vec![OsString::from("hi")]);
        assert!(w.env.iter().all(|(k, _)| k == "PATH"));
    }

    #[test]
    fn seatbelt_wraps_with_sandbox_exec() {
        let d = tempfile::tempdir().unwrap();
        let mut p = Policy::for_workspace(d.path());
        p.backend = BackendKind::Seatbelt;
        let w = wrap_command(&p, &argv(&["ls", "-l"])).unwrap();
        assert_eq!(w.program, OsString::from(seatbelt::SANDBOX_EXEC));
        assert_eq!(w.args[0], OsString::from("-p"));
        assert_eq!(
            &w.args[2..],
            [OsString::from("--"), "ls".into(), "-l".into()]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn landlock_wraps_via_helper() {
        let d = tempfile::tempdir().unwrap();
        let mut p = Policy::for_workspace(d.path());
        p.backend = BackendKind::Landlock;
        let w = wrap_command_with_helper(&p, &argv(&["ls"]), Path::new("/h")).unwrap();
        assert_eq!(w.program, OsString::from("/h"));
        assert_eq!(w.args[0], OsString::from(HELPER_ARG));
        assert_eq!(w.args[2], OsString::from("--"));
        let back: Policy = serde_json::from_str(w.args[1].to_str().unwrap()).unwrap();
        assert_eq!(back.write_roots, p.write_roots);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn landlock_unavailable_off_linux() {
        let d = tempfile::tempdir().unwrap();
        let mut p = Policy::for_workspace(d.path());
        p.backend = BackendKind::Landlock;
        assert!(wrap_command(&p, &argv(&["ls"])).is_err());
    }

    #[test]
    fn allowlist_and_empty_argv_rejected() {
        let d = tempfile::tempdir().unwrap();
        let mut p = Policy::for_workspace(d.path());
        p.backend = BackendKind::None;
        p.command_allowlist = Some(vec!["git".into()]);
        assert!(matches!(
            wrap_command(&p, &argv(&["rm"])),
            Err(SandboxError::CommandNotAllowed(_))
        ));
        assert!(matches!(
            wrap_command(&p, &[]),
            Err(SandboxError::EmptyArgv)
        ));
    }
}
