//! Resolve the server executable after nextest relocates an archived build.

pub fn oxsum() -> std::process::Command {
    // Cargo's compile-time path belongs to the build machine. nextest exports
    // the extracted executable at runtime; Cargo also supplies its runtime
    // variable on the workspace's supported Rust versions.
    let executable = std::env::var_os("NEXTEST_BIN_EXE_oxsum")
        .or_else(|| std::env::var_os("CARGO_BIN_EXE_oxsum"))
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_oxsum").into());
    std::process::Command::new(executable)
}
