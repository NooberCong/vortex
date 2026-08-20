; Installer hooks — the two moments where a file copy is not enough.
;
; Everything here delegates to `vortexd --register` / `--unregister`, which is ordinary
; Rust with tests behind it (`crates/vortex-setup`). NSIS is a poor place to decide
; anything: it cannot be unit-tested, it runs only when someone runs an installer, and the
; same work has to happen on macOS and Linux too, where there is no NSIS at all.
;
; Per-user install, so no `SetShellVarContext all` and no elevation anywhere below.

!macro NSIS_HOOK_PREINSTALL
  ; The daemon holds `vortexd.exe` open, and on Windows that is enough to fail the copy.
  ;
  ; A forced kill is safe here in a way it would not be in most products: every transfer
  ; is journalled continuously and resumes from `.vxpart.meta`, so the cost of killing it
  ; mid-download is a few re-fetched blocks (01 §Failure isolation). Asking politely is
  ; not an option — `vortexd` has no window to close and no console to signal.
  nsExec::Exec 'taskkill /F /IM vortexd.exe'
  Pop $0
  nsExec::Exec 'taskkill /F /IM vortex-host.exe'
  Pop $0
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Registers every browser it can, reports the rest, and returns non-zero only if none
  ; could be reached at all. Its output goes to the installer log.
  ;
  ; This also sets the login entry, from `settings.json` when there is one and from the
  ; default — on — when there is not. It has to happen here rather than at the daemon's
  ; next start: the uninstall hook below runs first on every update and every reinstall,
  ; and it takes the entry with it. Restoring it only when a daemon runs would mean a
  ; machine restarted straight after an update comes back with nothing running.
  nsExec::ExecToLog '"$INSTDIR\vortexd.exe" --register'
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Order matters. Unregister first — while `vortexd.exe` still exists to be run — and
  ; only then stop it. A registry value left pointing at a deleted executable is worse
  ; than no registration: the browser starts a host that is not there and reports it to
  ; the user as the extension being broken.
  nsExec::ExecToLog '"$INSTDIR\vortexd.exe" --unregister'
  Pop $0
  nsExec::Exec 'taskkill /F /IM vortexd.exe'
  Pop $0
  nsExec::Exec 'taskkill /F /IM vortex-host.exe'
  Pop $0
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; `%LOCALAPPDATA%\Vortex` stays. It holds the job list, the settings and the per-origin
  ; policy cache, and a reinstall that finds them is a reinstall that resumes what was
  ; running. Partial downloads were never here anyway — they live beside their
  ; destination file, so that moving one to another drive does not lose it.
!macroend
