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
        && !script_mutates_without_writes(cmd)
}

/// Commands the safe lists auto-allow that still change state without naming a write target:
/// `mkdir`/`touch` (an operand like `mkdir "$d"` would leave no trace in `protected_paths`) and
/// `git branch` with anything but listing options (create, delete, rename, move, set upstream).
fn script_mutates_without_writes(cmd: &str) -> bool {
    let Some(tree) = try_parse_shell(cmd) else {
        return true;
    };
    let Some(segments) = try_parse_word_only_commands_sequence(&tree, cmd) else {
        return true;
    };
    segments.iter().any(|segment| {
        let words = unwrap_wrappers(segment.words());
        is_safe_creation_command(words) || git_branch_mutates(words)
    })
}

/// `git branch` is on the shared read-only verb list, but only its listing forms are queries.
/// Global options mirror the ones the shared git check accepts before the verb (`-C <dir>`, `-C<dir>`, `--no-pager`, `-P`).
fn git_branch_mutates(words: &[String]) -> bool {
    if words.first().map(String::as_str) != Some("git") {
        return false;
    }
    let mut i = 1;
    while let Some(word) = words.get(i) {
        match word.as_str() {
            "-C" => i += 2,
            w if w.starts_with('-') => i += 1,
            _ => break,
        }
    }
    if words.get(i).map(String::as_str) != Some("branch") {
        return false;
    }
    let args = &words[i + 1..];
    let listing = args.iter().any(|arg| arg == "--list" || arg == "-l");
    let mut args = args.iter().map(String::as_str).peekable();
    while let Some(arg) = args.next() {
        match arg {
            "-a" | "--all" | "-r" | "--remotes" | "-v" | "-vv" | "--verbose" | "-l" | "--list"
            | "--show-current" | "-i" | "--ignore-case" | "--omit-empty" | "--color"
            | "--no-color" | "--column" | "--no-column" | "--abbrev" | "--no-abbrev" => {}
            "--contains" | "--no-contains" | "--merged" | "--no-merged" | "--points-at"
            | "--sort" | "--format" => {
                // The value is optional for some of these; never swallow a following option.
                args.next_if(|next| !next.starts_with('-'));
            }
            _ if arg.starts_with("--")
                && [
                    "--contains=",
                    "--no-contains=",
                    "--merged=",
                    "--no-merged=",
                    "--points-at=",
                    "--sort=",
                    "--format=",
                    "--color=",
                    "--column=",
                    "--abbrev=",
                ]
                .iter()
                .any(|prefix| arg.starts_with(prefix)) => {}
            // With `--list`, bare words are name patterns; without it they name a branch to create.
            _ if listing && !arg.starts_with('-') => {}
            _ => return true,
        }
    }
    false
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
            "git branch",
            "git branch -a -vv",
            "git branch --show-current",
            "git branch --list 'feat/*'",
            "git branch --contains HEAD -r",
            "git -C crates branch --merged main",
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
            "git branch feature-x",
            "git branch -D old",
            "git branch -m a b",
            "git branch -f main HEAD~3",
            "git branch -u origin/x",
            "git branch --edit-description",
            "git branch --contains -d old",
            "git -C crates branch new",
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
