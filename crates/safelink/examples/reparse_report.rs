//! Diagnostic: how does a reparse point on a mounted NTFS volume present
//! to Linux, and what does `classify()` say about it?
//!
//! A Linux-side `is_dir()` check is not safe proof that a path is an ordinary
//! directory: `ntfs-3g` presents junctions as symlinks and `ntfs3` has no
//! documented contract. This example is the tool that verifies that claim
//! against real Windows junctions.
//!
//! STRICTLY READ-ONLY: calls `classify()` and stdlib metadata only. It never
//! calls `remove_link()` and never writes. Safe to point at a live Windows
//! system volume.
//!
//! Usage -- pass paths to inspect, e.g. the legacy junctions Windows creates
//! at the root of its system volume:
//!
//!   cargo run --example reparse_report -- \
//!       /mnt/win/"Documents and Settings" \
//!       /mnt/win/Users/"All Users" \
//!       /mnt/win/Users            # a real directory, as a control
//!
//! Read the output as: a junction that reports `is_dir() == true` is exactly
//! the safety problem this tool exists to prevent.

use safelink::classify::classify;

fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: reparse_report <path>...");
        eprintln!("  Pass at least one path. Include a known-ordinary directory");
        eprintln!("  as a control so the contrast is visible.");
        std::process::exit(2);
    }

    println!(
        "{:<10} {:<9} {:<8} {:<30} path",
        "classify", "is_dir()", "is_link", "readlink"
    );
    println!("{}", "-".repeat(100));

    for p in &paths {
        let path = std::path::Path::new(p);

        let class = match classify(path) {
            Ok(c) => format!("{c:?}"),
            Err(e) => format!("ERR:{}", e.kind()),
        };
        // `is_dir()` follows links -- that is the whole point of the warning.
        let is_dir = path.is_dir();
        let is_link = path
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        let target = std::fs::read_link(path)
            .map(|t| t.display().to_string())
            .unwrap_or_else(|_| "-".into());

        println!("{class:<10} {is_dir:<9} {is_link:<8} {target:<30} {p}");
    }

    println!();
    println!("Any row with is_link=true and is_dir()=true is a link that an");
    println!("is_dir() check would mistake for an ordinary directory. Deleting");
    println!("such a path recursively would delete through it into the target.");
}
