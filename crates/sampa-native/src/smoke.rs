//! Headless cross-repo wiring check (`sampa --smoke`).
//!
//! Links all seven shared crates (ADR 0002) and round-trips a real shell over a PTY,
//! exercising `pty-core` + `sampa-shellint` and touching `sampa-config` +
//! `sampa-palette`. Kept as a fast, display-free sanity check for CI.

use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::Duration;

use anyhow::Result;
use pty_core::pty::{spawn, PtyEvent, SpawnConfig};
use sampa_shellint::OscScanner;

pub fn run() -> Result<()> {
    let cfg = sampa_config::Config::from_toml("")?;
    println!(
        "[config] font={} size={} scrollback={}",
        cfg.font.family, cfg.font.size, cfg.scrollback.lines
    );

    let path = std::env::var("PATH").unwrap_or_default();
    let exes = sampa_palette::list_executables(&path);
    println!("[palette] {} executables on $PATH", exes.len());

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let (tx, rx) = channel();
    let mut pty = spawn(
        SpawnConfig {
            shell: shell.clone(),
            args: vec![],
            cwd: None,
            cols: 80,
            rows: 24,
            env: vec![],
        },
        tx,
    )?;
    println!("[pty] spawned {shell} (pid {:?})", pty.pid());
    pty.write(b"echo hello-from-sampa-native && exit\n")?;

    let mut scanner = OscScanner::new();
    let mut saw_marker = false;
    loop {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(PtyEvent::Output(bytes)) => {
                let _ = scanner.feed(&bytes);
                if let Ok(s) = std::str::from_utf8(&bytes) {
                    saw_marker |= s.contains("hello-from-sampa-native");
                }
            }
            Ok(PtyEvent::Exit(info)) => {
                println!("[pty] exit: {} (success={})", info.detail, info.success);
                break;
            }
            Err(RecvTimeoutError::Timeout) => anyhow::bail!("timed out waiting for shell output"),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    anyhow::ensure!(saw_marker, "shell ran but the echo marker never came back");
    println!("[pty] round-trip verified — cross-repo core wiring OK");
    Ok(())
}
