//! Build metadata baked into the binary.
//!
//! `GET /api/v1/version` reports the commit the binary was built from. The
//! image build cannot run `git` — the context excludes `.git` — so the
//! Dockerfile passes the SHA in as `OXSUM_GIT_SHA` and this script forwards it
//! verbatim. A local build has the worktree and asks git itself; a build with
//! neither records `unknown` rather than fail, so a source tarball still
//! compiles.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=OXSUM_GIT_SHA");
    let sha = std::env::var("OXSUM_GIT_SHA")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(git_head)
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=OXSUM_GIT_SHA={sha}");
}

/// `git rev-parse HEAD` in the crate's worktree, or `None` when the source is a
/// tarball or git is absent.
fn git_head() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}
