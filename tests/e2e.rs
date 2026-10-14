use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use std::thread;

/// Serve one canned (status, body) per connection, in order. Returns the URL.
fn mock(responses: Vec<(u16, &'static str)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
    thread::spawn(move || {
        for (status, body) in responses {
            let (mut s, _) = listener.accept().unwrap();
            // Read the full request: headers, then Content-Length bytes of body.
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = s.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text[..end]
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + len || n == 0 {
                        break;
                    }
                }
            }
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(resp.as_bytes()).unwrap();
        }
    });
    url
}

fn jevgate(args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_jevgate"))
        .args(args)
        .env("JEV_API_KEY", "test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

const YES: &str = r#"{"model":"jev-1.13.0","answers":{"gate":{"type":"noul","noul":0.9}}}"#;
const NO: &str = r#"{"model":"jev-1.13.0","answers":{"gate":{"type":"noul","noul":0.1}}}"#;
const ECHO: [&str; 3] = ["sh", "-c", "cat; echo \" score=$JEVGATE_SCORE model=$JEVGATE_MODEL\"; exit 7"];

#[test]
fn pass_runs_command_with_input_and_env() {
    let url = mock(vec![(200, YES)]);
    let mut args = vec!["--url", &url, "q?", "--"];
    args.extend(ECHO);
    let out = jevgate(&args, "hello");
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello score=0.9 model=jev-1.13.0\n");
}

#[test]
fn pass_without_command_exits_0() {
    let url = mock(vec![(200, YES)]);
    assert_eq!(jevgate(&["--url", &url, "q?"], "hi").status.code(), Some(0));
}

#[test]
fn threshold_is_inclusive() {
    let url = mock(vec![(200, YES)]);
    assert_eq!(jevgate(&["--url", &url, "--threshold", "0.9", "q?"], "hi").status.code(), Some(0));
}

#[test]
fn reject_exits_1_silently_and_skips_command() {
    let url = mock(vec![(200, NO)]);
    let mut args = vec!["--url", &url, "q?", "--"];
    args.extend(ECHO);
    let out = jevgate(&args, "hello");
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
}

#[test]
fn retries_server_error() {
    let url = mock(vec![(500, "oops"), (200, YES)]);
    assert_eq!(jevgate(&["--url", &url, "q?"], "hi").status.code(), Some(0));
}

#[test]
fn client_error_exits_2_without_retry() {
    let url = mock(vec![(401, "bad key")]);
    let mut args = vec!["--url", &url, "q?", "--"];
    args.extend(ECHO);
    let out = jevgate(&args, "hello");
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("HTTP 401: bad key"));
}

#[test]
fn empty_input_exits_2() {
    assert_eq!(jevgate(&["--url", "http://127.0.0.1:1", "q?"], "  \n").status.code(), Some(2));
}

#[test]
fn openai_not_implemented() {
    let out = jevgate(&["--provider", "openai", "q?"], "hi");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn signal_killed_command_exits_128_plus_signal() {
    let url = mock(vec![(200, YES)]);
    let out = jevgate(&["--url", &url, "q?", "--", "sh", "-c", "kill -TERM $$"], "hi");
    assert_eq!(out.status.code(), Some(128 + 15));
}

#[test]
fn spawn_failure_exits_2() {
    let url = mock(vec![(200, YES)]);
    let out = jevgate(&["--url", &url, "q?", "--", "/nonexistent/cmd"], "hi");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn non_utf8_input_exits_2() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_jevgate"))
        .args(["--url", "http://127.0.0.1:1", "q?"])
        .stdin(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&[0xff, 0xfe]).unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(2));
}
