use std::io::Write;
use std::process::{Command, Stdio};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_lolcat")
}

#[test]
fn help_shows_usage() {
    let output = Command::new(binary())
        .arg("--help")
        .output()
        .expect("failed to run --help");
    assert!(
        output.status.success(),
        "non-zero exit: {:?}",
        output.status
    );
    let stdout = strip_ansi(&String::from_utf8_lossy(&output.stdout));
    assert!(stdout.contains("Usage: lolcat"), "help missing usage block");
}

#[test]
fn force_color_pipeline() {
    let mut child = Command::new(binary())
        .args(["-f", "--spread", "3", "--freq", "0.2"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn lolcat");

    {
        let mut stdin = child.stdin.take().expect("no stdin");
        stdin
            .write_all(b"hello\nworld\n")
            .expect("stdin write failed");
    }

    let output = child.wait_with_output().expect("failed to read output");
    assert!(output.status.success());
    let raw = String::from_utf8_lossy(&output.stdout);
    let body = strip_ansi(&raw);
    assert!(body.contains("hello"));
    assert!(
        raw.contains("\x1b[38;"),
        "expected ANSI color codes in output: {raw:?}"
    );
}

#[test]
fn version_reports_number() {
    let output = Command::new(binary())
        .arg("--version")
        .output()
        .expect("failed to run --version");
    assert!(output.status.success());
    let stdout = strip_ansi(&String::from_utf8_lossy(&output.stdout));
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "version output missing crate version"
    );
}

fn strip_ansi(input: &str) -> String {
    let mut chars = input.chars().peekable();
    let mut cleaned = String::with_capacity(input.len());
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.next() {
                Some('[') => {
                    while let Some(c) = chars.next() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{07}' {
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        cleaned.push(ch);
    }
    cleaned
}
