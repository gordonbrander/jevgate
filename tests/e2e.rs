use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;

/// Serve one canned (status, body) per connection, in order. Returns the URL.
fn mock(responses: Vec<(u16, &'static str)>) -> String {
    mock_recording(responses).0
}

/// Like `mock`, but also sends each raw request (headers and body) down the channel.
fn mock_recording(responses: Vec<(u16, &'static str)>) -> (String, Receiver<String>) {
    let (tx, rx) = mpsc::channel();
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
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + len || n == 0 {
                        break;
                    }
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(resp.as_bytes()).unwrap();
        }
    });
    (url, rx)
}

fn jevgate(args: &[&str], stdin: &str) -> Output {
    jevgate_with_key(args, stdin, Some("test"))
}

fn jevgate_with_key(args: &[&str], stdin: &str, key: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_jevgate"));
    match key {
        Some(k) => cmd.env("JEV_API_KEY", k),
        None => cmd.env_remove("JEV_API_KEY"),
    };
    let mut child = cmd
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

const YES: &str = r#"{"model":"jev-1.13.0","answers":{"gate":{"type":"noul","noul":0.9}}}"#;
const NO: &str = r#"{"model":"jev-1.13.0","answers":{"gate":{"type":"noul","noul":0.1}}}"#;
const ECHO: [&str; 3] = [
    "sh",
    "-c",
    "cat; echo \" score=$JEVGATE_SCORE model=$JEVGATE_MODEL\"; exit 7",
];

#[test]
fn pass_runs_command_with_input_and_env() {
    let url = mock(vec![(200, YES)]);
    let mut args = vec!["--url", &url, "q?", "--"];
    args.extend(ECHO);
    let out = jevgate(&args, "hello");
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hello score=0.9 model=jev-1.13.0\n"
    );
}

#[test]
fn pass_without_command_prints_input() {
    let url = mock(vec![(200, YES)]);
    let out = jevgate(&["--url", &url, "q?"], "hi\n");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(out.stdout, b"hi\n");
}

#[test]
fn quiet_pass_prints_nothing() {
    let url = mock(vec![(200, YES)]);
    let out = jevgate(&["--url", &url, "-q", "q?"], "hi");
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
}

#[test]
fn threshold_is_inclusive() {
    let url = mock(vec![(200, YES)]);
    assert_eq!(
        jevgate(&["--url", &url, "--threshold", "0.9", "q?"], "hi")
            .status
            .code(),
        Some(0)
    );
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
fn sends_key_as_bearer_token() {
    let (url, rx) = mock_recording(vec![(200, YES)]);
    let out = jevgate_with_key(&["--url", &url, "q?"], "hi", Some("sekrit"));
    assert_eq!(out.status.code(), Some(0));
    let req = rx.recv().unwrap().to_ascii_lowercase();
    assert!(req.contains("authorization: bearer sekrit\r\n"), "{req}");
}

#[test]
fn custom_url_without_key_sends_no_auth() {
    let (url, rx) = mock_recording(vec![(200, YES)]);
    let out = jevgate_with_key(&["--url", &url, "q?"], "hi", None);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        !rx.recv()
            .unwrap()
            .to_ascii_lowercase()
            .contains("authorization:")
    );
}

#[test]
fn default_url_without_key_exits_2() {
    let out = jevgate_with_key(&["q?"], "hi", None);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("JEV_API_KEY"));
}

#[test]
fn empty_input_exits_2() {
    assert_eq!(
        jevgate(&["--url", "http://127.0.0.1:1", "q?"], "  \n")
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn openai_not_implemented() {
    let out = jevgate(&["--provider", "openai", "q?"], "hi");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn signal_killed_command_exits_128_plus_signal() {
    let url = mock(vec![(200, YES)]);
    let out = jevgate(
        &["--url", &url, "q?", "--", "sh", "-c", "kill -TERM $$"],
        "hi",
    );
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
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&[0xff, 0xfe])
        .unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(2));
}
