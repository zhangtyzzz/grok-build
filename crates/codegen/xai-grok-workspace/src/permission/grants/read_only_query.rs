//! Fork (Team Leader mode): decide whether a Bash script is a pure read-only inspection.
//!
//! This is a delegation gate, not a permission decision: a team leader may still look around with the
//! built-in safe readers (`ls`, `cat`, `rg`, read-only `git`, …) while builds, tests and anything that
//! writes are handed to a subagent. The normal permission flow still runs for a script accepted here.
//! It reuses [`evaluate_bash`] so the reader list and its security floors stay the single source.

use super::{
    SegmentEvaluation, evaluate_bash, is_safe_creation_command, is_safe_write_sink,
    try_parse_shell, try_parse_word_only_commands_sequence, unwrap_wrappers,
};
use crate::permission::state::PermissionState;

/// True only when every command in `cmd` is on the built-in safe-reader lists and nothing is written.
///
/// Fails closed: scripts that cannot be decomposed (`$(…)`, background `&`), carry any
/// security finding, redirect to a real file, or create paths (`mkdir`/`touch`) answer `false`.
/// No user grant is consulted, so a session "always allow" never widens what a leader runs itself.
pub fn bash_is_read_only_query(cmd: &str) -> bool {
    let evaluation = evaluate_bash(cmd, &PermissionState::default(), true);
    matches!(
        evaluation.segments,
        SegmentEvaluation::AutoAllow {
            via_session_grant: false
        }
    ) && evaluation.assessment.is_empty()
        && !evaluation.redirect_write
        && evaluation
            .protected_paths
            .iter()
            .all(|path| is_safe_write_sink(path))
        && !script_creates_paths(cmd)
}

/// `mkdir`/`touch` are auto-allowed by the safe lists but are still writes; an operand the write
/// scan cannot extract (`mkdir "$d"`) would otherwise leave no trace in `protected_paths`.
fn script_creates_paths(cmd: &str) -> bool {
    let Some(tree) = try_parse_shell(cmd) else {
        return true;
    };
    let Some(segments) = try_parse_word_only_commands_sequence(&tree, cmd) else {
        return true;
    };
    segments
        .iter()
        .any(|segment| is_safe_creation_command(unwrap_wrappers(segment.words())))
}

#[cfg(test)]
mod tests {
    use super::bash_is_read_only_query;

    #[test]
    fn plain_readers_are_read_only() {
        for cmd in [
            "ls -la",
            "cat Cargo.toml",
            "pwd",
            "rg -n TODO src",
            "git status",
            "git log --oneline -5",
            "git diff HEAD~1",
            "cat a.txt | grep foo | head -5",
            "cd crates && ls",
            "ls missing 2>/dev/null",
            "wc -l src/lib.rs",
            "cat <<EOF\nhi\nEOF",
        ] {
            assert!(bash_is_read_only_query(cmd), "{cmd} should be read-only");
        }
    }

    #[test]
    fn builds_tests_and_unknown_programs_are_not_read_only() {
        for cmd in [
            "cargo build",
            "cargo test -p foo",
            "npm install",
            "make",
            "python script.py",
            "./run.sh",
            "ls && cargo check",
        ] {
            assert!(!bash_is_read_only_query(cmd), "{cmd} must be delegated");
        }
    }

    #[test]
    fn writes_are_not_read_only() {
        for cmd in [
            "echo hi > out.txt",
            "cat a >> b",
            "mkdir build",
            "touch marker",
            "mkdir \"$d\"",
            "rm -rf target",
            "git commit -m x",
            "git push",
            "git checkout -b feature",
            "sed -i s/a/b/ file",
            "cat a | tee b",
            "cat > out.txt <<EOF\nhi\nEOF",
        ] {
            assert!(!bash_is_read_only_query(cmd), "{cmd} must be delegated");
        }
    }

    #[test]
    fn opaque_shell_fails_closed() {
        for cmd in ["cat $(ls)", "ls &", "bash -c 'ls'", "rg --pre cat foo"] {
            assert!(!bash_is_read_only_query(cmd), "{cmd:?} must fail closed");
        }
    }
}
