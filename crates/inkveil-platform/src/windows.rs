//! Windows implementations.

use std::ffi::c_void;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, GlobalFree, HANDLE, HGLOBAL, HLOCAL, HWND, LPARAM, LRESULT, LocalFree,
            RECT, WPARAM,
        },
        Graphics::Gdi::{
            GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
        },
        Security::{
            ACL,
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SDDL_REVISION_1, SE_KERNEL_OBJECT, SetSecurityInfo,
            },
            DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
            TokenUser,
        },
        System::{
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, OpenClipboard,
                RegisterClipboardFormatW, SetClipboardData,
            },
            ErrorReporting::{WER_FAULT_REPORTING_FLAG_NOHEAP, WerSetFlags},
            LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories},
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            Ole::{CF_DIB, CF_UNICODETEXT},
            RemoteDesktop::{
                WTS_SESSIONSTATE_LOCK, WTSFreeMemory, WTSINFOEXW, WTSQuerySessionInformationW,
                WTSSessionInfoEx,
            },
            SystemServices::{
                PROCESS_MITIGATION_EXTENSION_POINT_DISABLE_POLICY,
                PROCESS_MITIGATION_IMAGE_LOAD_POLICY,
            },
            Threading::{
                GetCurrentProcess, OpenProcessToken, ProcessExtensionPointDisablePolicy,
                ProcessImageLoadPolicy, SetProcessMitigationPolicy,
            },
        },
        UI::{
            HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow},
            WindowsAndMessaging::{
                CallWindowProcW, DefWindowProcW, GWL_EXSTYLE, GWL_STYLE, GWLP_WNDPROC,
                GetClientRect, GetPropW, GetWindowLongW, GetWindowPlacement, GetWindowRect,
                IsIconic, IsZoomed, RemovePropW, SIZE_MAXIMIZED, SWP_NOACTIVATE, SWP_NOZORDER,
                SetPropW, SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPlacement,
                SetWindowPos, USER_DEFAULT_SCREEN_DPI, WDA_EXCLUDEFROMCAPTURE, WINDOW_EX_STYLE,
                WINDOW_STYLE, WINDOWPLACEMENT, WM_DPICHANGED, WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE,
                WM_NCDESTROY, WM_SIZE, WNDPROC,
            },
        },
    },
    core::{BOOL, PCWSTR, PWSTR, w},
};

use crate::{PlatformError, Support};

pub(crate) fn exclude_from_capture(
    window: &impl HasWindowHandle,
) -> Result<Support, PlatformError> {
    let hwnd = extract_hwnd(window)?;
    // SAFETY: `hwnd` comes from a live window owned by this process, borrowed for this call.
    unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) }
        .map_err(|e| PlatformError::new("SetWindowDisplayAffinity", e))?;
    tracing::info!("window excluded from screen capture");
    Ok(Support::Full)
}

fn extract_hwnd(window: &impl HasWindowHandle) -> Result<HWND, PlatformError> {
    let handle = window
        .window_handle()
        .map_err(|e| PlatformError::new("getting the window handle", e))?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err(PlatformError::new(
            "getting the window handle",
            "expected a Win32 window handle",
        ));
    };
    Ok(HWND(handle.hwnd.get() as *mut c_void))
}

/// Builds the DACL for this process, as SDDL:
/// - `P`: protected, so no inherited entries widen it;
/// - `SY` (`LocalSystem`) keeps full access;
/// - `OW` (owner rights) replaces the owner's implicit `READ_CONTROL | WRITE_DAC`, so no process
///   can rewrite this DACL to grant itself more;
/// - the current user gets only synchronize, query limited information and terminate. Without
///   `PROCESS_VM_READ`, `PROCESS_VM_WRITE` or `PROCESS_CREATE_THREAD`, other programs running as
///   the same user cannot read memory or start threads through a process handle, but Task Manager
///   can still end the process.
///
/// This narrows, not closes, the same-user attack surface: thread handles, window hooks and COM
/// hijacking do not need a process handle. `apply_mitigations` covers some of those.
fn process_sddl() -> Result<Vec<u16>, PlatformError> {
    const LIMITED: &str = "0x00101001";
    let user = current_user_sid()?;
    let sddl = format!("D:P(A;;GA;;;SY)(A;;{LIMITED};;;OW)(A;;{LIMITED};;;{user})");
    Ok(sddl.encode_utf16().chain([0]).collect())
}

/// The current user's SID as a string such as `S-1-5-21-…`.
fn current_user_sid() -> Result<String, PlatformError> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle is always valid; `token` receives a handle closed below.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) }
        .map_err(|e| PlatformError::new("OpenProcessToken", e))?;
    let result = token_user_sid(token);
    // SAFETY: `token` was opened above and is not used afterwards.
    let _ = unsafe { CloseHandle(token) };
    result
}

fn token_user_sid(token: HANDLE) -> Result<String, PlatformError> {
    let mut needed = 0u32;
    // SAFETY: a size query with no buffer; fails with "insufficient buffer" and sets `needed`.
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &raw mut needed) };
    // u64 elements keep the buffer aligned for TOKEN_USER.
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buffer` holds at least `needed` bytes and is suitably aligned.
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            needed,
            &raw mut needed,
        )
    }
    .map_err(|e| PlatformError::new("GetTokenInformation", e))?;
    // SAFETY: on success the buffer starts with a TOKEN_USER whose SID points into the buffer,
    // which stays alive until the end of this function.
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut text = PWSTR::null();
    // SAFETY: `sid` is valid (see above); `text` receives a LocalAlloc'd string freed below.
    unsafe { ConvertSidToStringSidW(sid, &raw mut text) }
        .map_err(|e| PlatformError::new("ConvertSidToStringSidW", e))?;
    // SAFETY: `text` is a valid NUL-terminated wide string from the call above.
    let result = unsafe { text.to_string() }.map_err(|e| PlatformError::new("reading the SID", e));
    // SAFETY: allocated by ConvertSidToStringSidW with LocalAlloc; not used afterwards.
    unsafe {
        LocalFree(Some(HLOCAL(text.0.cast())));
    }
    result
}

/// Mitigations against code being loaded into the process (DLL planting, hooks, injection).
/// Each is applied separately; a failure is logged and the others still apply.
fn apply_mitigations() {
    // DLLs loaded at run time are searched for in System32 only, never in the folder the app
    // was started from (where a malicious DLL could have been dropped next to it).
    // SAFETY: plain flag change for this process.
    if let Err(e) = unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) } {
        tracing::warn!("SetDefaultDllDirectories failed: {e}");
    }

    // Refuse images from network shares or with a low integrity label, and prefer System32
    // copies of system DLLs.
    let mut image_load = PROCESS_MITIGATION_IMAGE_LOAD_POLICY::default();
    image_load.Anonymous.Flags = 0b111; // NoRemoteImages | NoLowMandatoryLabelImages | PreferSystem32Images
    set_mitigation("image load", ProcessImageLoadPolicy, &image_load);

    // Block legacy extension points that inject DLLs into every process: AppInit DLLs, global
    // window hooks (SetWindowsHookEx with a DLL), Winsock layered providers and legacy IMEs.
    // Modern (Text Services Framework) input methods are unaffected.
    let mut extension_points = PROCESS_MITIGATION_EXTENSION_POINT_DISABLE_POLICY::default();
    extension_points.Anonymous.Flags = 0b1; // DisableExtensionPoints
    set_mitigation(
        "extension point",
        ProcessExtensionPointDisablePolicy,
        &extension_points,
    );

    // If Windows Error Reporting captures a crash dump, leave the heap (and so note text) out.
    // SAFETY: plain flag change for this process.
    if let Err(e) = unsafe { WerSetFlags(WER_FAULT_REPORTING_FLAG_NOHEAP) } {
        tracing::warn!("WerSetFlags failed: {e}");
    }
}

fn set_mitigation<T>(
    name: &str,
    policy: windows::Win32::System::Threading::PROCESS_MITIGATION_POLICY,
    value: &T,
) {
    // SAFETY: `value` is the policy structure matching `policy`, and lives across the call.
    let result = unsafe {
        SetProcessMitigationPolicy(
            policy,
            std::ptr::from_ref(value).cast(),
            std::mem::size_of::<T>(),
        )
    };
    if let Err(e) = result {
        tracing::warn!("{name} mitigation failed: {e}");
    }
}

pub(crate) fn harden_process() -> Result<Support, PlatformError> {
    apply_mitigations();
    let sddl = process_sddl()?;
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `sddl` is a valid NUL-terminated wide string that outlives the call, and
    // `descriptor` receives a LocalAlloc'd buffer that is freed below.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &raw mut descriptor,
            None,
        )
    }
    .map_err(|e| PlatformError::new("building the process security descriptor", e))?;

    let result = apply_dacl(descriptor);
    // SAFETY: `descriptor` was allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW
    // with LocalAlloc and is not used after this point.
    unsafe {
        LocalFree(Some(HLOCAL(descriptor.0)));
    }
    result?;
    tracing::info!("process access restricted");
    Ok(Support::Full)
}

fn apply_dacl(descriptor: PSECURITY_DESCRIPTOR) -> Result<(), PlatformError> {
    let mut present = BOOL(0);
    let mut defaulted = BOOL(0);
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: `descriptor` is a valid self-relative descriptor; the out pointers are valid locals.
    unsafe {
        GetSecurityDescriptorDacl(
            descriptor,
            &raw mut present,
            &raw mut dacl,
            &raw mut defaulted,
        )
    }
    .map_err(|e| PlatformError::new("reading the DACL", e))?;

    // SAFETY: GetCurrentProcess returns a pseudo handle that is always valid for this process;
    // `dacl` points into `descriptor`, which outlives this call.
    let status = unsafe {
        SetSecurityInfo(
            HANDLE(GetCurrentProcess().0),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(dacl),
            None,
        )
    };
    if status.is_err() {
        return Err(PlatformError::new(
            "SetSecurityInfo",
            windows::core::Error::from(status.to_hresult()),
        ));
    }
    Ok(())
}

/// `WTS_CURRENT_SESSION`: the session this process runs in.
const CURRENT_SESSION: u32 = u32::MAX;

pub(crate) fn is_session_locked() -> Option<bool> {
    let mut buffer = PWSTR::null();
    let mut bytes = 0u32;
    // SAFETY: queries the current session on the local server; on success `buffer` points to a
    // WTSINFOEXW allocated by WTS and freed below.
    unsafe {
        WTSQuerySessionInformationW(
            None,
            CURRENT_SESSION,
            WTSSessionInfoEx,
            &raw mut buffer,
            &raw mut bytes,
        )
    }
    .ok()?;
    let locked = if (bytes as usize) >= std::mem::size_of::<WTSINFOEXW>() {
        // SAFETY: the buffer holds at least one WTSINFOEXW (size checked above). It is read
        // unaligned because the API only promises a byte buffer. Level 1 is the only level
        // Windows defines, so reading that union member is valid.
        let info = unsafe { std::ptr::read_unaligned(buffer.0.cast::<WTSINFOEXW>()) };
        let flags = unsafe { info.Data.WTSInfoExLevel1.SessionFlags };
        Some(u32::try_from(flags).ok() == Some(WTS_SESSIONSTATE_LOCK))
    } else {
        None
    };
    // SAFETY: `buffer` was allocated by WTSQuerySessionInformationW and is not used afterwards.
    unsafe { WTSFreeMemory(buffer.0.cast()) };
    locked
}

pub(crate) fn write_private_clipboard_text(text: &str) -> Result<u32, PlatformError> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    write_private_clipboard(&[(u32::from(CF_UNICODETEXT.0), bytemuck_u16(&wide))])
}

/// Puts a picture on the clipboard as PNG (for apps that read it) and as a device-independent
/// bitmap (for everything else), with the same privacy markers as text.
pub(crate) fn write_private_clipboard_image(png: &[u8], dib: &[u8]) -> Result<u32, PlatformError> {
    // SAFETY: `w!` gives a static NUL-terminated wide string.
    let png_format = unsafe { RegisterClipboardFormatW(w!("PNG")) };
    if png_format == 0 {
        return Err(PlatformError::new("RegisterClipboardFormatW", "returned 0"));
    }
    write_private_clipboard(&[(png_format, png), (u32::from(CF_DIB.0), dib)])
}

/// Writes `entries` (format, bytes) to the clipboard after the privacy markers.
fn write_private_clipboard(entries: &[(u32, &[u8])]) -> Result<u32, PlatformError> {
    // SAFETY: opening the clipboard with no owner window is allowed; it is closed by the guard.
    unsafe { OpenClipboard(None) }.map_err(|e| PlatformError::new("OpenClipboard", e))?;
    let guard = ClipboardGuard;

    // SAFETY: we hold the clipboard open.
    unsafe { EmptyClipboard() }.map_err(|e| PlatformError::new("EmptyClipboard", e))?;

    // The markers go first, so the text never sits on the clipboard without them. Documented
    // markers honoured by Windows clipboard history, cloud clipboard and well-behaved clipboard
    // managers. A DWORD 0 means "no"; the monitor marker's presence alone is the signal.
    let result = (|| {
        for (name, value) in [
            (w!("ExcludeClipboardContentFromMonitorProcessing"), 0u32),
            (w!("CanIncludeInClipboardHistory"), 0u32),
            (w!("CanUploadToCloudClipboard"), 0u32),
        ] {
            // SAFETY: `name` is a static NUL-terminated wide string.
            let format = unsafe { RegisterClipboardFormatW(name) };
            if format == 0 {
                return Err(PlatformError::new("RegisterClipboardFormatW", "returned 0"));
            }
            set_clipboard_data(format, &value.to_ne_bytes())?;
        }
        for (format, bytes) in entries {
            set_clipboard_data(*format, bytes)?;
        }
        Ok(())
    })();

    if let Err(error) = result {
        // Never leave content behind without its markers.
        // SAFETY: we still hold the clipboard open.
        let _ = unsafe { EmptyClipboard() };
        return Err(error);
    }
    drop(guard);
    // SAFETY: no preconditions.
    Ok(unsafe { GetClipboardSequenceNumber() })
}

/// The clipboard's change counter.
pub(crate) fn clipboard_sequence() -> u32 {
    // SAFETY: no preconditions.
    unsafe { GetClipboardSequenceNumber() }
}

/// Empties the clipboard.
pub(crate) fn clear_clipboard() -> Result<(), PlatformError> {
    // SAFETY: as in `write_private_clipboard_text`.
    unsafe { OpenClipboard(None) }.map_err(|e| PlatformError::new("OpenClipboard", e))?;
    let _guard = ClipboardGuard;
    // SAFETY: we hold the clipboard open.
    unsafe { EmptyClipboard() }.map_err(|e| PlatformError::new("EmptyClipboard", e))
}

/// Copies `bytes` into global memory and hands it to the clipboard (which must be open).
fn set_clipboard_data(format: u32, bytes: &[u8]) -> Result<(), PlatformError> {
    let memory = global_copy(bytes)?;
    // SAFETY: the caller holds the clipboard; on success the system owns `memory`.
    if let Err(e) = unsafe { SetClipboardData(format, Some(HANDLE(memory.0))) } {
        // SAFETY: ownership did not pass to the system, so the memory is still ours to free.
        let _ = unsafe { GlobalFree(Some(memory)) };
        return Err(PlatformError::new("SetClipboardData", e));
    }
    Ok(())
}

/// Closes the clipboard when dropped.
struct ClipboardGuard;

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        // SAFETY: only constructed after a successful OpenClipboard.
        let _ = unsafe { CloseClipboard() };
    }
}

/// Copies `bytes` into a new movable global memory block, as the clipboard requires.
fn global_copy(bytes: &[u8]) -> Result<HGLOBAL, PlatformError> {
    // SAFETY: plain allocation; checked for failure below.
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) }
        .map_err(|e| PlatformError::new("GlobalAlloc", e))?;
    // SAFETY: `memory` is a fresh movable block of at least `bytes.len()` bytes; it is locked
    // while written and unlocked right after.
    unsafe {
        let target = GlobalLock(memory).cast::<u8>();
        if target.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err(PlatformError::new("GlobalLock", "returned null"));
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
        let _ = GlobalUnlock(memory);
    }
    Ok(memory)
}

fn bytemuck_u16(words: &[u16]) -> &[u8] {
    // SAFETY: u16 has no padding and any byte pattern is a valid u8; the length is exact.
    unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), std::mem::size_of_val(words)) }
}

const HOOK_PROP: PCWSTR = w!("InkveilWindowHookState");
/// Smallest window the adjustments below produce, in logical pixels.
const MIN_LOGICAL_WIDTH: i32 = 640;
const MIN_LOGICAL_HEIGHT: i32 = 420;
/// Share of a monitor's work area a window keeps at least and at most when it changes monitors.
const MIN_WORK_RATIO: f64 = 0.35;
const MAX_WORK_RATIO: f64 = 0.88;

/// Per-window state of the hook, owned by the window property `HOOK_PROP`.
struct WindowHookState {
    prev_wndproc: WNDPROC,
    /// The monitor the window was last moved on or maximized on.
    monitor: HMONITOR,
    move_start: Option<MoveStart>,
}

/// The window when a move or resize started.
#[derive(Clone, Copy)]
struct MoveStart {
    width: i32,
    height: i32,
    monitor: HMONITOR,
    dpi: u32,
}

impl MoveStart {
    fn of(hwnd: HWND) -> Option<Self> {
        // SAFETY: `hwnd` is live.
        let rect = if unsafe { IsZoomed(hwnd) }.as_bool() {
            // Dragging a maximized window restores it to this size first.
            placement(hwnd)?.rcNormalPosition
        } else {
            let mut rect = RECT::default();
            // SAFETY: `hwnd` is live and `rect` is a valid out pointer.
            unsafe { GetWindowRect(hwnd, &raw mut rect) }.ok()?;
            rect
        };
        Some(Self {
            width: rect.right - rect.left,
            height: rect.bottom - rect.top,
            // SAFETY: `hwnd` is live.
            monitor: unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) },
            dpi: window_dpi(hwnd),
        })
    }
}

pub(crate) fn configure_window(window: &impl HasWindowHandle) -> Result<Support, PlatformError> {
    let hwnd = extract_hwnd(window)?;
    // SAFETY: `hwnd` is a live window of this thread; `HOOK_PROP` is a static wide string.
    if !unsafe { GetPropW(hwnd, HOOK_PROP) }.0.is_null() {
        return Ok(Support::Full);
    }
    let state = Box::into_raw(Box::new(WindowHookState {
        prev_wndproc: None,
        // SAFETY: `hwnd` is a live window.
        monitor: unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) },
        move_start: None,
    }));
    // The state goes in place before the hook, so the hook always finds it.
    // SAFETY: `hwnd` is live; the property holds `state` until `WM_NCDESTROY` frees it.
    if let Err(e) = unsafe { SetPropW(hwnd, HOOK_PROP, Some(HANDLE(state.cast()))) } {
        // SAFETY: `state` came from `Box::into_raw` above and was never shared.
        drop(unsafe { Box::from_raw(state) });
        return Err(PlatformError::new("SetPropW", e));
    }
    // SAFETY: `hwnd` belongs to this thread, and `hook_wndproc` has the window procedure
    // signature. The previous procedure is stored before any message can reach the hook, since
    // messages to this window are only delivered on this thread.
    let prev = unsafe {
        SetWindowLongPtrW(
            hwnd,
            GWLP_WNDPROC,
            (hook_wndproc as *const () as usize).cast_signed(),
        )
    };
    if prev == 0 {
        // SAFETY: undoing the property set above; nothing else has seen `state`.
        unsafe {
            let _ = RemovePropW(hwnd, HOOK_PROP);
            drop(Box::from_raw(state));
        }
        return Err(PlatformError::new("SetWindowLongPtrW", "returned 0"));
    }
    // SAFETY: `prev` is the non-zero window procedure Windows returned; `WNDPROC` is an optional
    // function pointer of the same size. `state` is valid and only used on this thread.
    unsafe { (*state).prev_wndproc = std::mem::transmute::<isize, WNDPROC>(prev) };
    Ok(Support::Full)
}

unsafe extern "system" fn hook_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state = hook_state(hwnd);
    if state.is_null() {
        // SAFETY: default handling with the arguments Windows passed in.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    // SAFETY: `state` is the live hook state; it is only touched on this window's thread.
    let prev = unsafe { (*state).prev_wndproc };
    if msg == WM_NCDESTROY {
        // SAFETY: the window is going away: unhook, free the state (nothing reads it after
        // this), then let GPUI run its own cleanup.
        unsafe {
            let _ = RemovePropW(hwnd, HOOK_PROP);
            if let Some(proc) = prev {
                SetWindowLongPtrW(
                    hwnd,
                    GWLP_WNDPROC,
                    (proc as *const () as usize).cast_signed(),
                );
            }
            drop(Box::from_raw(state));
            return CallWindowProcW(prev, hwnd, msg, wparam, lparam);
        }
    }
    if msg == WM_ENTERSIZEMOVE {
        // SAFETY: as above.
        unsafe { (*state).move_start = MoveStart::of(hwnd) };
    }

    // SAFETY: GPUI's window procedure with the arguments Windows passed in.
    let result = unsafe { CallWindowProcW(prev, hwnd, msg, wparam, lparam) };

    // GPUI may have re-entered this procedure; look the state up again.
    let state = hook_state(hwnd);
    if state.is_null() {
        return result;
    }
    match msg {
        WM_DPICHANGED => fix_maximized_client_size(hwnd, prev),
        WM_EXITSIZEMOVE => keep_share_after_move(hwnd, state),
        WM_SIZE if wparam.0 == SIZE_MAXIMIZED as usize => fit_restore_size(hwnd, state),
        _ => {}
    }
    result
}

fn hook_state(hwnd: HWND) -> *mut WindowHookState {
    // SAFETY: `hwnd` is the window the current message is for.
    unsafe { GetPropW(hwnd, HOOK_PROP) }.0.cast()
}

/// After a DPI change GPUI sizes a maximized window's content to the whole work area, ignoring
/// the title bar and frame, so the bottom of the app (the status bar) ends up off screen. Tells
/// GPUI the real client size.
fn fix_maximized_client_size(hwnd: HWND, prev: WNDPROC) {
    // SAFETY: `hwnd` is live.
    if !unsafe { IsZoomed(hwnd) }.as_bool() || unsafe { IsIconic(hwnd) }.as_bool() {
        return;
    }
    let mut client = RECT::default();
    // SAFETY: `hwnd` is live and `client` is a valid out pointer.
    if unsafe { GetClientRect(hwnd, &raw mut client) }.is_err() {
        return;
    }
    let width = (client.right - client.left).clamp(0, 0xFFFF);
    let height = (client.bottom - client.top).clamp(0, 0xFFFF);
    #[allow(clippy::cast_sign_loss)]
    let size = LPARAM((((height as usize) << 16) | width as usize).cast_signed());
    // SAFETY: a `WM_SIZE` exactly as Windows would send it, straight to GPUI's procedure.
    unsafe {
        let _ = CallWindowProcW(prev, hwnd, WM_SIZE, WPARAM(SIZE_MAXIMIZED as usize), size);
    }
}

/// After the user drags the window to another monitor, keeps its share of the screen. Windows
/// keeps its size in logical pixels instead, so a window that suits a large monitor covers
/// nearly all of a small high-DPI laptop screen. Done when the drag ends, so the window never
/// jumps away from the pointer, and only for moves: a size the user dragged to stays.
fn keep_share_after_move(hwnd: HWND, state: *mut WindowHookState) {
    // SAFETY: `state` is the live hook state on its window's thread.
    let Some(start) = (unsafe { (*state).move_start.take() }) else {
        return;
    };
    // SAFETY: `hwnd` is live.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    // SAFETY: as above.
    unsafe { (*state).monitor = monitor };
    // SAFETY: `hwnd` is live.
    if start.monitor == monitor
        || unsafe { IsZoomed(hwnd) }.as_bool()
        || unsafe { IsIconic(hwnd) }.as_bool()
    {
        return;
    }
    let mut rect = RECT::default();
    // SAFETY: `hwnd` is live and `rect` is a valid out pointer.
    if unsafe { GetWindowRect(hwnd, &raw mut rect) }.is_err() {
        return;
    }
    let dpi = window_dpi(hwnd);
    let logical = |pixels: i32, dpi: u32| {
        f64::from(pixels) * f64::from(USER_DEFAULT_SCREEN_DPI) / f64::from(dpi.max(1))
    };
    // Only moves: a size the user dragged to stays.
    let resized = (logical(start.width, start.dpi) - logical(rect.right - rect.left, dpi)).abs()
        > 8.
        || (logical(start.height, start.dpi) - logical(rect.bottom - rect.top, dpi)).abs() > 8.;
    if resized {
        return;
    }
    let (Some(old_work), Some(work)) =
        (monitor_work_area(start.monitor), monitor_work_area(monitor))
    else {
        return;
    };
    let (min_w, min_h) = min_outer_size(hwnd, dpi);
    #[allow(clippy::cast_possible_truncation)]
    let scaled = |size: i32, old_work: i32, work: i32, min: i32| {
        let share =
            (f64::from(size) / f64::from(old_work.max(1))).clamp(MIN_WORK_RATIO, MAX_WORK_RATIO);
        ((share * f64::from(work)).round() as i32)
            .max(min)
            .min(work)
    };
    let width = scaled(
        start.width,
        old_work.right - old_work.left,
        work.right - work.left,
        min_w,
    );
    let height = scaled(
        start.height,
        old_work.bottom - old_work.top,
        work.bottom - work.top,
        min_h,
    );
    // Where it was dropped, moved inside the work area if it now sticks out.
    let left = rect
        .left
        .clamp(work.left, (work.right - width).max(work.left));
    let top = rect
        .top
        .clamp(work.top, (work.bottom - height).max(work.top));
    // SAFETY: `hwnd` is live; this only moves and sizes it.
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            None,
            left,
            top,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
}

/// When the window is maximized on another monitor than before, makes sure its restore size fits
/// there, so un-maximizing on a small screen does not give a second full screen.
fn fit_restore_size(hwnd: HWND, state: *mut WindowHookState) {
    // SAFETY: `hwnd` is live.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    // SAFETY: `state` is the live hook state on its window's thread.
    if std::mem::replace(unsafe { &mut (*state).monitor }, monitor) == monitor {
        return;
    }
    let Some(work) = monitor_work_area(monitor) else {
        return;
    };
    let Some(mut wp) = placement(hwnd) else {
        return;
    };
    let (work_w, work_h) = (work.right - work.left, work.bottom - work.top);
    let (min_w, min_h) = min_outer_size(hwnd, window_dpi(hwnd));
    #[allow(clippy::cast_possible_truncation)]
    let max = |work: i32, min: i32| {
        ((f64::from(work) * MAX_WORK_RATIO).round() as i32).max(min.min(work))
    };
    let (max_w, max_h) = (max(work_w, min_w), max(work_h, min_h));
    let normal = wp.rcNormalPosition;
    let (w, h) = (normal.right - normal.left, normal.bottom - normal.top);
    if w <= max_w && h <= max_h {
        return;
    }
    let (w, h) = (w.min(max_w), h.min(max_h));
    // Restore positions are in workspace coordinates, which match screen coordinates unless the
    // taskbar is at the top or left; centring keeps the window on screen either way.
    let left = work.left + (work_w - w) / 2;
    let top = work.top + (work_h - h) / 2;
    wp.rcNormalPosition = RECT {
        left,
        top,
        right: left + w,
        bottom: top + h,
    };
    // SAFETY: `wp` came from `GetWindowPlacement`; only the restore rectangle changed.
    let _ = unsafe { SetWindowPlacement(hwnd, &raw const wp) };
}

fn placement(hwnd: HWND) -> Option<WINDOWPLACEMENT> {
    let mut wp = WINDOWPLACEMENT {
        length: u32::try_from(std::mem::size_of::<WINDOWPLACEMENT>()).unwrap_or(0),
        ..Default::default()
    };
    // SAFETY: `wp.length` is set to the structure's size.
    unsafe { GetWindowPlacement(hwnd, &raw mut wp) }.ok()?;
    Some(wp)
}

fn window_dpi(hwnd: HWND) -> u32 {
    // SAFETY: `hwnd` is a live window.
    match unsafe { GetDpiForWindow(hwnd) } {
        0 => USER_DEFAULT_SCREEN_DPI,
        dpi => dpi,
    }
}

fn monitor_work_area(monitor: HMONITOR) -> Option<RECT> {
    let mut info = MONITORINFO {
        cbSize: u32::try_from(std::mem::size_of::<MONITORINFO>()).unwrap_or(0),
        ..Default::default()
    };
    // SAFETY: `info.cbSize` is set to the structure's size.
    unsafe { GetMonitorInfoW(monitor, &raw mut info) }
        .as_bool()
        .then_some(info.rcWork)
}

/// The outer size (with title bar and frame) of the smallest window, at `dpi`.
fn min_outer_size(hwnd: HWND, dpi: u32) -> (i32, i32) {
    let scale = f64::from(dpi) / f64::from(USER_DEFAULT_SCREEN_DPI);
    #[allow(clippy::cast_possible_truncation)]
    let (width, height) = (
        (f64::from(MIN_LOGICAL_WIDTH) * scale).round() as i32,
        (f64::from(MIN_LOGICAL_HEIGHT) * scale).round() as i32,
    );
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    // SAFETY: `hwnd` is a live window.
    let style = WINDOW_STYLE(unsafe { GetWindowLongW(hwnd, GWL_STYLE) }.cast_unsigned());
    // SAFETY: as above.
    let ex_style = WINDOW_EX_STYLE(unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) }.cast_unsigned());
    // SAFETY: `rect` is a valid in/out pointer.
    let _ = unsafe { AdjustWindowRectExForDpi(&raw mut rect, style, false, ex_style, dpi) };
    (
        (rect.right - rect.left).max(width),
        (rect.bottom - rect.top).max(height),
    )
}
