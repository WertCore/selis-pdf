#![no_main]

use libfuzzer_sys::fuzz_target;
use selis_sandbox::{Budget, Surface};
use selis_pdf_cos::parse;

fuzz_target!(|data: &[u8]| {
    let budget = Budget::profile(Surface::Fuzz);
    let _ = parse(data, &budget);
});
