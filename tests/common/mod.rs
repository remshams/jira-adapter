// Shared CLI integration-test helpers. Include with `mod common;` and use
// `common::jcli`. Functions unused by a particular test binary are expected,
// hence the blanket allow.
#![allow(dead_code)]

use std::path::Path;
use std::process::{Command, Output};

/// Runs the jcli binary in a unique empty working directory with a cleared
/// environment, so tests never see the host's HOME, XDG_CONFIG_HOME, or a
/// stray `.env`, and each call gets its own configuration lookup.
pub fn jcli(args: &[&str], env: &[(&str, &str)]) -> Output {
    let dir = tempfile::tempdir().unwrap();
    jcli_in(dir.path(), args, env)
}

/// Same as `jcli`, but runs in the given directory instead of a throwaway
/// temp directory, for tests that need files in the child's working
/// directory (for example download output).
pub fn jcli_in(directory: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jcli"));
    command.args(args).current_dir(directory).env_clear();
    for (key, value) in env {
        command.env(key, value);
    }
    if let Some(profile_file) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile_file);
    }
    command.output().expect("jcli should start")
}
