use std::convert::TryFrom;
use std::env;
use std::ffi::OsString;
use std::process::{Command, ExitCode, ExitStatus};

const SCCACHE_ENV: &str = "GROK_SCCACHE_BIN";

fn run(program: &OsString, args: &[OsString]) -> std::io::Result<ExitStatus> {
    Command::new(program).args(args).status()
}

fn main() -> ExitCode {
    let mut wrapper_args = env::args_os().skip(1);
    let Some(compiler) = wrapper_args.next() else {
        eprintln!("sccache fallback wrapper requires the compiler path");
        return ExitCode::FAILURE;
    };
    let compiler_args: Vec<_> = wrapper_args.collect();

    let Some(sccache) = env::var_os(SCCACHE_ENV) else {
        eprintln!("{SCCACHE_ENV} is not set; invoking compiler directly");
        return status_code(run(&compiler, &compiler_args));
    };

    let mut sccache_args = Vec::with_capacity(compiler_args.len() + 1);
    sccache_args.push(compiler.clone());
    sccache_args.extend(compiler_args.iter().cloned());
    match run(&sccache, &sccache_args) {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            eprintln!(
                "sccache exited with {status}; retrying this compiler invocation without the cache"
            );
            status_code(run(&compiler, &compiler_args))
        }
        Err(error) => {
            eprintln!(
                "could not invoke sccache ({error}); retrying this compiler invocation without the cache"
            );
            status_code(run(&compiler, &compiler_args))
        }
    }
}

fn status_code(result: std::io::Result<ExitStatus>) -> ExitCode {
    match result {
        Ok(status) => status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map_or(ExitCode::FAILURE, ExitCode::from),
        Err(error) => {
            eprintln!("could not invoke compiler: {error}");
            ExitCode::FAILURE
        }
    }
}
