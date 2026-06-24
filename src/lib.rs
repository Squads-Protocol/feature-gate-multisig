// unwrap/expect/panic are idiomatic in unit tests; the panic-prevention lints
// stay strict for production code only.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::panic_in_result_fn
    )
)]

pub mod commands;
pub mod constants;
pub mod feature_gate_program;
pub mod output;
pub mod provision;
pub mod squads;
pub mod utils;
pub mod verification;
