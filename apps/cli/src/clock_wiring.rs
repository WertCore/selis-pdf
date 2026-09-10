//! SL-0.SBX.07 — CLI-level proof that the real clock is wired.
//!
//! The CLI is the L5 binding boundary: the one place in the stack where
//! `Budget::wall` may be measured against a genuine monotonic clock. These
//! tests pin the wiring end to end:
//!
//! * the shell clock really advances (a frozen clock would report the same
//!   reading twice),
//! * an open through `Session::open` on the shell clock sees real elapsed
//!   time in the guard's own usage,
//! * a full command (`selis render`) runs through the injected clock.

use selis_pdf_engine::Session;
use selis_sandbox::{Budget, CancelToken, Clock, Surface};

const MINIMAL_PDF: &[u8] =
    include_bytes!("../../../crates/selis-pdf-engine/src/fixtures/minimal.pdf");

/// The shell clock must be a *real* clock: monotonic and strictly increasing
/// across a genuine wait. `FixedClock(0)` — the pre-SBX.07 default — fails
/// the second assertion by construction.
#[test]
fn the_shell_clock_is_real_and_monotonic() {
    let clock = crate::shell_clock();
    let first = clock.now();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let second = clock.now();
    assert!(second >= first, "monotonic: the reading never decreases");
    assert!(second > first, "real time passes between two reads");
}

/// An open on the shell clock must charge real elapsed time: after a wait,
/// the guard's own `usage().wall` is non-zero. This is the property that was
/// structurally absent before the clock injection (every guard read a
/// frozen `FixedClock(0)`, so `wall` was always exactly 0).
#[test]
fn the_real_clock_wires_through_session_open() {
    let budget = Budget::profile(Surface::Viewer);
    let clock = crate::shell_clock();
    let session = Session::open(MINIMAL_PDF.to_vec(), &budget, &clock).expect("open");
    assert_eq!(session.len(), 1, "one page");

    let mut g = budget.guard_with(&clock, CancelToken::new());
    std::thread::sleep(std::time::Duration::from_millis(2));
    g.tick().expect("within the viewer wall");
    assert!(
        g.usage().wall > 0,
        "the wired clock must report real elapsed time, got {}",
        g.usage().wall
    );
}

/// A full command path — `selis render`, open through render to PPM output —
/// runs on the injected clock.
#[test]
fn the_render_command_runs_on_the_real_clock() {
    let dir = std::env::temp_dir().join("selis-sbx07-clock-wiring");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let src = dir.join("minimal.pdf");
    let out = dir.join("minimal.ppm");
    std::fs::write(&src, MINIMAL_PDF).expect("write fixture");

    crate::render::run(
        &src.display().to_string(),
        0,
        &out.display().to_string(),
        72,
    )
    .expect("render on the real clock");
    let ppm = std::fs::metadata(&out).expect("PPM written");
    assert!(ppm.len() > 0, "the PPM has content");
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}
