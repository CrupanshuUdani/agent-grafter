# safelink

A cross-platform crate for creating and safely removing directory links
(NTFS junctions on Windows, symlinks on POSIX) without ever traversing
through the link to delete the files it points to.

It is the engine under the `agent-grafter` CLI. `agent-grafter` decides
*what* should exist where; `safelink` does the per-path work and refuses
anything it can't do safely.

## How the pieces fit

```
Entry { source, dest, op }        what to do, declared by the caller (op.rs)
        │
        ▼
materialize(entry, probe, opts)   does one entry, returns the op it applied
        │
        ├─ DirectoryLink ──► create_dir_link   junction (Windows) / symlink (POSIX)
        ├─ FileSymlink ────► probe says yes? → file symlink
        │                    probe says no?  → copy_atomic (the one allowed fallback)
        └─ FileCopy ───────► copy_atomic       temp file beside dest, then rename

classify(path)     what is at a path: a directory link, a file link, or not a link
remove_link(path)  removes a link object only; refuses anything that isn't one
```

`classify` and `remove_link` are the removal side: callers classify a
destination first, and only ever hand links to `remove_link`.

## Public API

| Module | Item | What it does |
|---|---|---|
| `op` | `LinkOp` | The operation for one path. `DirectoryLink` (directories only, admin-free), `FileCopy` (atomic copy), or `FileSymlink` (real file symlink, used only when the system allows it). There is no "auto" variant: every entry says what it wants. |
| `op` | `Entry` | One unit of work: `source` (never modified or deleted), `dest`, and `op`. |
| `materialize` | `materialize(entry, probe, options)` | Puts one `Entry` in place and returns the `LinkOp` it **actually** applied. The only allowed difference from the declared op is `FileSymlink` becoming `FileCopy` when the probe says symlinks aren't available. A missing source is refused before `dest` is touched. |
| `materialize` | `MaterializeOptions` | `create_missing_parents` (default `true`). With `false`, a missing parent directory is refused and named, and nothing is created. |
| `capability` | `SymlinkProbe` | Trait answering "can a file symlink be created near this path?". It asks about the destination's filesystem, since support differs per filesystem (exFAT, FAT32 and some SMB mounts have none). |
| `capability` | `RealProbe` | Answers by trying to create a symlink next to the destination and cleaning it up. |
| `capability` | `ForcedProbe(bool)` | Always gives the answer you pass. Used in tests to reach the copy fallback on machines where symlinks work. |
| `create` | `create_dir_link(target, link)` | Creates a directory link. An NTFS junction on Windows (no admin rights needed, works across local volumes), a symlink on POSIX. Never a Windows directory symlink, which would need Developer Mode or elevation. |
| `copy` | `copy_atomic(src, dest)` | Copies through a temp file in `dest`'s own directory, then renames it into place. A crash mid-copy leaves the old `dest` intact. |
| `classify` | `classify(path)` → `LinkClass` | Says what is at `path` without following it: `DirLink` (junction or directory symlink), `FileLink`, or `NotALink`. On Windows it checks the link's own attributes first, so a junction whose target is gone is still a `DirLink`. |
| `remove` | `remove_link(path)` | Removes the link object itself. Never descends into the target, and returns an error for anything that `classify` says isn't a link. |
| `paths` | `normalize_for_classification(path)` | Strips a trailing separator. On POSIX, `lstat("link/")` follows the link, so `classify` would otherwise describe the target instead of the link. |
| `paths` | `verbatim(path)` | On Windows, adds the `\\?\` prefix to paths over 260 characters so they don't fail with a misleading "path not found". A no-op on POSIX. |

## Guarantees

- Removing a link never deletes what it points to. No code in `src/` calls
  `std::fs::remove_dir_all`, and a test scans the source to keep it that way.
- A missing source never leads to changes at the destination.
- The declared op is honoured exactly, except for the one `FileSymlink` →
  `FileCopy` fallback, and `materialize`'s return value tells the caller when
  that happened.

## Example

```rust,no_run
use std::path::PathBuf;

use safelink::capability::RealProbe;
use safelink::materialize::{MaterializeOptions, materialize};
use safelink::op::{Entry, LinkOp};

fn main() -> std::io::Result<()> {
    let entry = Entry {
        source: PathBuf::from("/shared/agent/AGENTS.md"),
        dest: PathBuf::from("/home/you/.claude/AGENTS.md"),
        op: LinkOp::FileSymlink,
    };
    let applied = materialize(&entry, &RealProbe, &MaterializeOptions::default())?;
    if applied == LinkOp::FileCopy {
        println!("symlinks aren't available here, so this is a copy");
    }
    Ok(())
}
```

`examples/smoke.rs` is a runnable version you can point at real paths
(`cargo run --example smoke -- dir|file <source> <dest>`).

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
