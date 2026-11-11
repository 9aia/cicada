//! cicada — compat CLI driver. The engine is the `cicada` lib
//! (src/lib.rs); this binary is just `cicada::run()` mapped to a
//! process exit code.

fn main() -> std::process::ExitCode {
    // Engine codes are 0/1 — anything outside u8 collapses to
    // failure rather than wrapping.
    std::process::ExitCode::from(u8::try_from(cicada::run()).unwrap_or(1))
}
