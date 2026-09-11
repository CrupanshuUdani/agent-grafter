# safelink

A cross-platform crate for creating and safely removing directory links
(NTFS junctions on Windows, symlinks on POSIX) without ever traversing
through the link to delete the files it points to.

## A note on CI coverage

GitHub-hosted Windows runners are documented to run "as administrators with
User Account Control (UAC) disabled." An administrator holds
`SeCreateSymbolicLinkPrivilege`, so **CI cannot validate the capability-
detection path**: `RealProbe` returns `true` there, while a standard user
with Developer Mode off gets `false`.

CI proves junction creation, classification and safe removal — on a real
Windows kernel, which is the whole point of the three-OS matrix. It does
**not** prove the symlink-to-copy fallback fires for real users, and it does
**not** prove `RealProbe` reports `false` for a genuine standard account.
Those are covered by `ForcedProbe(false)` unit tests plus a manual run of
`cargo run --example smoke` on a non-elevated Windows account with
Developer Mode off.

A green CI run is evidence for what CI can actually exercise. It is not
evidence that a non-admin Windows user gets the same behavior.
