//! Standalone landlock helper: `hk-sandbox-exec --hk-sandbox-exec <policy-json> -- <argv...>`.
//! Same entry as `hk_sandbox::maybe_run_helper`, for hosts that prefer a sibling binary.

fn main() {
    // Invoked as helper iff args match; otherwise print usage and fail.
    hk_sandbox::maybe_run_helper();
    // `maybe_run_helper` only returns when the args are not a helper request.
    std::process::exit(2);
}
