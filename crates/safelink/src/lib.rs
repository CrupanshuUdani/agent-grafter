//! Explicit link materialization with a hard safety guarantee:
//! removing a link never deletes through it into the target.

/// Loud marker so a green run on a non-Windows host is never mistaken for
/// validation of the Windows link semantics. See Global Constraint 6.
#[cfg(not(windows))]
#[test]
fn windows_link_semantics_are_not_validated_on_this_platform() {
    eprintln!(
        "\n\
         ======================================================================\n\
         SKIPPED: Windows junction semantics are NOT exercised on this host.\n\
         A green run here is NOT evidence the Windows path works.\n\
         Only the `windows-latest` CI job is evidence for Windows claims,\n\
         and even it runs elevated (see Global Constraint 7).\n\
         ======================================================================\n"
    );
}

pub mod capability;
pub mod classify;
pub mod create;
pub mod op;
pub mod remove;
