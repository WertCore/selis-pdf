//! Minimal driver for the Linearize tool operation.
//!
//! Usage: `cargo run -p selis-pdf-tools --example linearize -- INPUT.pdf OUTPUT.pdf [--xref-streams]`
//!
//! Exposes the library operation so it can be exercised from the shell and by
//! the independent structural checkers in `tools/`. The optional
//! `--xref-streams` flag selects the compressed (PDF 1.5) xref/object-stream
//! writer; the default is the classic PDF 1.4 xref.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 && args.len() != 4 {
        eprintln!("usage: linearize INPUT.pdf OUTPUT.pdf [--xref-streams]");
        std::process::exit(2);
    }
    let (Some(input), Some(output)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: linearize INPUT.pdf OUTPUT.pdf [--xref-streams]");
        std::process::exit(2);
    };
    let mode = if args.get(3).map(String::as_str) == Some("--xref-streams") {
        selis_pdf_tools::LinearizeMode::Compressed
    } else {
        selis_pdf_tools::LinearizeMode::Classic
    };
    match selis_pdf_tools::linearize_file_with_mode(input, output, mode) {
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
