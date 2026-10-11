//! Manual smoke test. Run this on the Windows boot as a standard,
//! non-elevated user with Developer Mode OFF:
//!
//!   cargo run --example smoke -- dir  C:\path\to\source C:\path\to\dest
//!   cargo run --example smoke -- file C:\path\to\src.md C:\path\to\dest.md

use safelink::capability::RealProbe;
use safelink::materialize::{MaterializeOptions, materialize};
use safelink::op::{Entry, LinkOp};
use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: smoke <dir|file> <source> <dest>");
        std::process::exit(2);
    }

    let op = match args[1].as_str() {
        "dir" => LinkOp::DirectoryLink,
        "file" => LinkOp::FileSymlink,
        other => {
            eprintln!("unknown mode {other:?}; expected 'dir' or 'file'");
            std::process::exit(2);
        }
    };

    let entry = Entry {
        source: PathBuf::from(&args[2]),
        dest: PathBuf::from(&args[3]),
        op,
    };

    println!("symlink capability at the destination: {}", {
        use safelink::capability::SymlinkProbe;
        // Asked about the destination specifically -- on exFAT/FAT32 this is
        // `false` even though /tmp would say `true`.
        RealProbe.can_create_file_symlink(&entry.dest)
    });

    // Defaults: the smoke test is a manual one-shot against paths the
    // operator just typed, so auto-creating the destination's parents is the
    // helpful behavior. Strictness is a manifest-runner concern.
    let applied = materialize(&entry, &RealProbe, &MaterializeOptions::default())?;
    println!("declared: {:?}\napplied:  {:?}", entry.op, applied);
    if applied != entry.op {
        println!("(downgraded — this is the expected fallback, not an error)");
    }
    Ok(())
}
