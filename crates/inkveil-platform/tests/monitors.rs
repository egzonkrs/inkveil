//! Moves a real window between two monitors with different sizes or scaling, with
//! [`inkveil_platform::configure_window`] installed, and checks where it ends up.
//!
//! It needs a second monitor, so it only runs on request:
//! `cargo test -p inkveil-platform --test monitors -- --ignored --nocapture`
//!
//! The window is fully transparent and never takes the keyboard, so the test does not get in the
//! way of whoever is using the computer.

#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::print_stdout, unsafe_code)]

use std::{
    num::NonZeroIsize,
    time::{Duration, Instant},
};

use raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, GetDpiForWindow,
                MDT_EFFECTIVE_DPI, SetThreadDpiAwarenessContext,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetWindowRect,
                IsZoomed, LWA_ALPHA, MONITORINFOF_PRIMARY, MSG, PM_REMOVE, PeekMessageW,
                RegisterClassW, SC_RESTORE, SW_MAXIMIZE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE,
                SWP_NOZORDER, SendMessageW, SetLayeredWindowAttributes, SetWindowPos, ShowWindow,
                TranslateMessage, WINDOW_EX_STYLE, WM_SYSCOMMAND, WNDCLASSW, WS_EX_LAYERED,
                WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW,
            },
        },
    },
    core::{BOOL, w},
};

/// A monitor: its work area in physical pixels and its scale.
#[derive(Debug, Clone, Copy)]
struct Monitor {
    work: RECT,
    dpi: u32,
    primary: bool,
}

impl Monitor {
    fn logical(&self, pixels: i32) -> f64 {
        f64::from(pixels) * 96. / f64::from(self.dpi)
    }

    fn physical(&self, logical: f64) -> i32 {
        #[allow(clippy::cast_possible_truncation)]
        let pixels = (logical * f64::from(self.dpi) / 96.).round() as i32;
        pixels
    }
}

fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn add(monitor: HMONITOR, _: HDC, _: *mut RECT, list: LPARAM) -> BOOL {
        let list = unsafe { &mut *(list.0 as *mut Vec<Monitor>) };
        let mut info = MONITORINFO {
            cbSize: u32::try_from(std::mem::size_of::<MONITORINFO>()).unwrap(),
            ..Default::default()
        };
        let (mut dpi, mut unused) = (0, 0);
        unsafe {
            let _ = GetMonitorInfoW(monitor, &raw mut info);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut dpi, &raw mut unused);
        }
        list.push(Monitor {
            work: info.rcWork,
            dpi,
            primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
        });
        BOOL(1)
    }
    let mut list: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(add),
            LPARAM(std::ptr::from_mut(&mut list) as isize),
        );
    }
    list
}

struct Window(HWND);

impl HasWindowHandle for Window {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let handle = Win32WindowHandle::new(NonZeroIsize::new(self.0.0 as isize).unwrap());
        // SAFETY: the window lives until the test destroys it, after its last use.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(handle)) })
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

unsafe extern "system" fn plain_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// An invisible window, as large as `rect`, with the hook installed.
fn window_at(rect: RECT) -> Window {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap();
        let class = WNDCLASSW {
            lpfnWndProc: Some(plain_wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("InkveilMonitorTest"),
            ..Default::default()
        };
        RegisterClassW(&raw const class);
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WINDOW_EX_STYLE(0),
            w!("InkveilMonitorTest"),
            w!("Inkveil monitor test"),
            WS_OVERLAPPEDWINDOW,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .unwrap();
        // Fully transparent: shown to Windows, invisible to the person at the screen.
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA).unwrap();
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let window = Window(hwnd);
        inkveil_platform::configure_window(&window).unwrap();
        pump();
        window
    }
}

/// Handles the window's messages for a moment, as an app's message loop would.
fn pump() {
    let until = Instant::now() + Duration::from_millis(250);
    while Instant::now() < until {
        let mut msg = MSG::default();
        while unsafe { PeekMessageW(&raw mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&raw const msg);
                DispatchMessageW(&raw const msg);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn rect_of(window: &Window) -> RECT {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(window.0, &raw mut rect) }.unwrap();
    rect
}

/// Moves the window to `left`, `top` without changing its size, as the keyboard or snapping
/// would, and lets the hook react.
fn move_to(window: &Window, left: i32, top: i32) {
    let rect = rect_of(window);
    unsafe {
        SetWindowPos(
            window.0,
            None,
            left,
            top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
        .unwrap();
    }
    pump();
}

fn assert_inside(rect: RECT, work: RECT) {
    assert!(
        rect.left >= work.left
            && rect.top >= work.top
            && rect.right <= work.right
            && rect.bottom <= work.bottom,
        "{rect:?} is not inside {work:?}"
    );
}

fn width(rect: RECT) -> i32 {
    rect.right - rect.left
}

fn height(rect: RECT) -> i32 {
    rect.bottom - rect.top
}

/// The two monitors to move between, the primary first, if they differ in scale or size.
fn pair() -> Option<(Monitor, Monitor)> {
    // Real pixels and scales, as Inkveil sees them.
    unsafe {
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let list = monitors();
    let first = *list.iter().find(|m| m.primary)?;
    let second = *list.iter().find(|m| !m.primary)?;
    println!("primary {first:?}\nsecond {second:?}");
    Some((first, second))
}

#[test]
#[ignore = "needs two monitors"]
fn a_window_keeps_its_size_and_always_fits_when_it_changes_monitors() {
    let Some((laptop, monitor)) = pair() else {
        println!("only one monitor; nothing to check");
        return;
    };
    // A typical window: 1089 x 663 logical, on the primary monitor.
    let (logical_w, logical_h) = (1089., 663.);
    let start = RECT {
        left: laptop.work.left + 40,
        top: laptop.work.top + 40,
        right: laptop.work.left + 40 + laptop.physical(logical_w),
        bottom: laptop.work.top + 40 + laptop.physical(logical_h),
    };
    let window = window_at(start);
    let before = rect_of(&window);
    println!("on the primary: {before:?}");

    // To the other monitor: the same size in logical pixels, inside its work area.
    move_to(&window, monitor.work.left + 60, monitor.work.top + 60);
    let there = rect_of(&window);
    println!("moved over: {there:?} at {} dpi", unsafe {
        GetDpiForWindow(window.0)
    });
    assert_inside(there, monitor.work);
    let expected_w = monitor
        .physical(laptop.logical(width(before)))
        .min(width(monitor.work));
    assert!(
        (width(there) - expected_w).abs() <= 16,
        "{} wide, expected about {expected_w}",
        width(there)
    );

    // And back: the size it had at the start.
    move_to(&window, laptop.work.left + 80, laptop.work.top + 80);
    let back = rect_of(&window);
    println!("moved back: {back:?}");
    assert_inside(back, laptop.work);
    assert!((width(back) - width(before)).abs() <= 16);
    assert!((height(back) - height(before)).abs() <= 16);
}

#[test]
#[ignore = "needs two monitors"]
fn a_window_too_big_for_the_other_monitor_is_made_to_fit() {
    let Some((first, second)) = pair() else {
        println!("only one monitor; nothing to check");
        return;
    };
    // Nearly the whole of the monitor with more room in logical pixels.
    let (big, small) = if first.logical(width(first.work)) >= second.logical(width(second.work)) {
        (first, second)
    } else {
        (second, first)
    };
    let start = RECT {
        left: big.work.left,
        top: big.work.top,
        right: big.work.left + width(big.work) * 95 / 100,
        bottom: big.work.top + height(big.work) * 95 / 100,
    };
    let window = window_at(start);
    move_to(&window, small.work.left + 20, small.work.top + 20);
    let there = rect_of(&window);
    println!("from the larger monitor to the smaller one: {there:?}");
    assert_inside(there, small.work);
    assert!(width(there) < width(small.work) && height(there) < height(small.work));
}

#[test]
#[ignore = "needs two monitors"]
fn restoring_a_window_maximized_on_the_other_monitor_gives_a_size_that_fits_there() {
    let Some((laptop, monitor)) = pair() else {
        println!("only one monitor; nothing to check");
        return;
    };
    let start = RECT {
        left: laptop.work.left + 40,
        top: laptop.work.top + 40,
        right: laptop.work.left + 40 + width(laptop.work) * 85 / 100,
        bottom: laptop.work.top + 40 + height(laptop.work) * 85 / 100,
    };
    let window = window_at(start);
    let before = rect_of(&window);
    unsafe {
        let _ = ShowWindow(window.0, SW_MAXIMIZE);
    }
    pump();
    assert!(unsafe { IsZoomed(window.0) }.as_bool());
    // Over to the other monitor while maximized, as Win+Shift+Arrow does.
    unsafe {
        SetWindowPos(
            window.0,
            None,
            monitor.work.left,
            monitor.work.top,
            width(monitor.work),
            height(monitor.work),
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
        .unwrap();
    }
    pump();
    // The restore button.
    unsafe {
        SendMessageW(
            window.0,
            WM_SYSCOMMAND,
            Some(WPARAM(SC_RESTORE as usize)),
            Some(LPARAM(0)),
        );
    }
    pump();
    let restored = rect_of(&window);
    println!("restored on the other monitor: {restored:?}");
    assert!(!unsafe { IsZoomed(window.0) }.as_bool());
    assert_inside(restored, monitor.work);
    assert!(width(restored) < width(monitor.work) && height(restored) < height(monitor.work));
    let logical_before = laptop.logical(width(before));
    let logical_after = monitor.logical(width(restored));
    assert!(
        logical_after <= logical_before + 16.,
        "{logical_after} logical wide after, {logical_before} before"
    );
}
