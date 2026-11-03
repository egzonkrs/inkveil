//! Checks that private clipboard writes really reach other programs.
//!
//! These replace the clipboard's contents, so they only run on request:
//! `cargo test -p inkveil-platform --test clipboard -- --ignored --test-threads=1`

#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use std::process::Command;

/// What another process (PowerShell) sees on the clipboard.
fn clipboard_from_another_process() -> String {
    let out = Command::new("powershell")
        .args(["-NoProfile", "-Command", "Get-Clipboard -Raw"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
#[ignore = "replaces the clipboard"]
fn private_text_can_be_pasted_elsewhere() {
    let text = "inkveil clipboard check 7Q4M-XK2P";
    let sequence = inkveil_platform::write_private_clipboard_text(text)
        .unwrap()
        .unwrap();
    assert_eq!(clipboard_from_another_process(), text);
    // Clearing only happens while nothing else was copied since.
    assert!(inkveil_platform::clear_clipboard_if_unchanged(sequence).unwrap());
    assert_eq!(clipboard_from_another_process(), "");
}
