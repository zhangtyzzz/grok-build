//! Standalone worker for hermetic protocol/host integration tests.
fn main() {
    if xai_grok_tools::code_mode::run_worker().is_err() {
        std::process::exit(1);
    }
}
