//! Windows environment facts `doctor` reports but never acts on: whether the
//! current process is elevated, and whether Developer Mode is on.
//!
//! Injectable on purpose, mirroring `safelink::capability::SymlinkProbe` /
//! `RealProbe` / `ForcedProbe` and the reasoning that file documents: the
//! real answer can only be observed on an actual Windows machine (this dev
//! machine is Linux, and GitHub's Windows runners are documented to run
//! elevated with UAC disabled -- see Global Constraint 7), so the
//! *reporting* logic that formats these two facts into `doctor`'s output
//! must be testable independently of the real OS query, via a forced
//! implementation.
//!
//! [`WindowsFacts`] and [`ForcedWindowsFacts`] are therefore plain, portable
//! Rust with no Windows dependency and compile on every platform -- only
//! [`RealWindowsFacts`], which actually calls into `windows-sys`, is
//! `#[cfg(windows)]`-gated, both because it is meaningless anywhere else and
//! because `windows-sys` itself is only a dependency on Windows
//! (`Cargo.toml`'s `[target.'cfg(windows)'.dependencies]`). `doctor::diagnose`
//! only constructs and calls `RealWindowsFacts` inside its own
//! `#[cfg(windows)]` block, so this section of the report is skipped
//! entirely on other platforms rather than printing a stub answer that
//! means nothing there.

/// Facts about the current process/session that only make sense on Windows.
///
/// Report-only: `doctor` never uses either fact to change its exit code or
/// to influence what `apply` does. A standard, non-elevated user with
/// Developer Mode off is the fully supported, intended case this whole
/// project exists for -- junctions need neither -- so these are a
/// diagnostic courtesy, not a pass/fail verdict.
///
/// Each method returns `Option<bool>`, not `bool`: `None` means the query
/// itself did not complete (a token query or registry read failed for a
/// reason other than "the value is absent, which means off"). Collapsing
/// that into a bare `false` would print a *fact* ("developer mode: off")
/// that was never actually observed -- exactly what AGENTS.md's Boundaries
/// forbid ("do not turn assumptions into facts"), and the same honesty
/// standard `report_backup_roots`'s own `could not inspect {path}: {e}`
/// line and `report_plan_drift`'s `plan.notes` already hold check 3 and
/// check 5 to. `doctor::report_windows_environment` renders `None` as its
/// own third line, not as either boolean state.
pub trait WindowsFacts {
    /// Whether the current process token is elevated. `None` if this could
    /// not be determined.
    fn is_elevated(&self) -> Option<bool>;
    /// Whether Developer Mode is enabled, per
    /// `AllowDevelopmentWithoutDevLicense` under
    /// `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock`.
    /// `None` if this could not be determined.
    fn developer_mode_enabled(&self) -> Option<bool>;
}

/// Queries the real OS state via `windows-sys`.
#[cfg(windows)]
pub struct RealWindowsFacts;

#[cfg(windows)]
impl WindowsFacts for RealWindowsFacts {
    fn is_elevated(&self) -> Option<bool> {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::Security::{
            GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        // Safety: `GetCurrentProcess` returns a pseudo-handle that need not
        // (and must not) be closed; `token`, once successfully opened by
        // `OpenProcessToken`, is a real handle and is always closed below,
        // on every path, including the `GetTokenInformation` failure path.
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                // The query itself did not complete -- unknown, not "not
                // elevated". See this module's `WindowsFacts` doc comment.
                return None;
            }

            let mut elevation = TOKEN_ELEVATION::default();
            let mut returned_len: u32 = 0;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                &mut elevation as *mut TOKEN_ELEVATION as *mut core::ffi::c_void,
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut returned_len,
            ) != 0;

            CloseHandle(token);

            if ok {
                Some(elevation.TokenIsElevated != 0)
            } else {
                None
            }
        }
    }

    fn developer_mode_enabled(&self) -> Option<bool> {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;

        use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
        use windows_sys::Win32::System::Registry::{
            HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW,
        };

        fn wide(s: &str) -> Vec<u16> {
            OsStr::new(s)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        }

        let subkey = wide(r"SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock");
        let value = wide("AllowDevelopmentWithoutDevLicense");
        let mut data: u32 = 0;
        let mut size: u32 = std::mem::size_of::<u32>() as u32;

        // Safety: `subkey` and `value` are valid, NUL-terminated wide
        // strings kept alive for the whole call; `data`/`size` describe a
        // single `u32` output buffer matching `RRF_RT_REG_DWORD`.
        // `HKEY_LOCAL_MACHINE` is a predefined pseudo-handle, not one this
        // code opens or must close -- `RegGetValueW` opens the named
        // subkey internally.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                &mut data as *mut u32 as *mut core::ffi::c_void,
                &mut size,
            )
        };

        match status {
            // The value exists and was read: the actual, observed fact.
            _ if status == ERROR_SUCCESS => Some(data != 0),
            // The key or value is simply absent. Per Microsoft's own
            // documented default, that means Developer Mode has never been
            // turned on -- a genuinely observed "off", not a guess.
            ERROR_FILE_NOT_FOUND => Some(false),
            // Anything else (access denied, a truncated/wrong-typed value,
            // ...) is a query that did not complete. Reporting `false` here
            // would print a fact never actually observed.
            _ => None,
        }
    }
}

/// Forces fixed answers, mirroring `safelink::capability::ForcedProbe`. Test
/// facing in spirit, public so this crate's own tests can use it. This is
/// what makes `doctor::report_windows_environment`'s formatting logic
/// unit-testable on any platform, including this Linux dev machine, without
/// touching the real OS.
pub struct ForcedWindowsFacts {
    pub elevated: Option<bool>,
    pub developer_mode: Option<bool>,
}

impl WindowsFacts for ForcedWindowsFacts {
    fn is_elevated(&self) -> Option<bool> {
        self.elevated
    }

    fn developer_mode_enabled(&self) -> Option<bool> {
        self.developer_mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_facts_report_what_they_were_given() {
        let both_on = ForcedWindowsFacts {
            elevated: Some(true),
            developer_mode: Some(true),
        };
        assert_eq!(both_on.is_elevated(), Some(true));
        assert_eq!(both_on.developer_mode_enabled(), Some(true));

        let both_off = ForcedWindowsFacts {
            elevated: Some(false),
            developer_mode: Some(false),
        };
        assert_eq!(both_off.is_elevated(), Some(false));
        assert_eq!(both_off.developer_mode_enabled(), Some(false));

        let unknown = ForcedWindowsFacts {
            elevated: None,
            developer_mode: None,
        };
        assert_eq!(unknown.is_elevated(), None);
        assert_eq!(unknown.developer_mode_enabled(), None);
    }

    /// Same shape as `capability.rs`'s
    /// `real_probe_is_callable_and_does_not_panic`, and for the same reason:
    /// this dev machine is Linux and cannot exercise `RealWindowsFacts` at
    /// all, and even a Windows CI runner cannot validate the "not elevated"
    /// branch (Global Constraint 7: GitHub's Windows runners are documented
    /// to run elevated with UAC disabled). This asserts only that the real
    /// query is callable and does not panic -- nothing about the actual
    /// values, which are environment-dependent and would be a falsehood to
    /// assert either way here.
    ///
    /// Per Constraint 7 and the task report this module ships with: this is
    /// unverified on a real Windows machine until confirmed there (Gate B).
    #[cfg(windows)]
    #[test]
    fn real_facts_are_callable_and_do_not_panic() {
        let facts = RealWindowsFacts;
        let _ = facts.is_elevated();
        let _ = facts.developer_mode_enabled();
    }
}
