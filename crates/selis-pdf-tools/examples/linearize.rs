//! Minimal driver for the Linearize tool operation.
//!
//! Usage: `cargo run -p selis-pdf-tools --example linearize -- INPUT.pdf OUTPUT.pdf`
//!
//! Exposes the library operation so it can be exercised from the shell and by
//! the independent structural checker in `tools/check_linearization.py`.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: linearize INPUT.pdf OUTPUT.pdf");
        std::process::exit(2);
    }
    let (Some(input), Some(output)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: linearize INPUT.pdf OUTPUT.pdf");
        std::process::exit(2);
    };
    match selis_pdf_tools::linearize_file(input, output) {
        Ok((report, verification)) => {
            eprintln!(
                "objects={} {} -> {} bytes, /L={}, /N={}, verdict={}",
                report.objects,
                report.in_bytes,
                report.out_bytes,
                report.declared_length,
                report.declared_pages,
                verification.verdict
            );
        }
        Err(e) => {
            eprintln!("linearize failed: {e}");
            std::process::exit(1);
        }
    }
}
