#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use optive::vm::{OutputSink, Vm};

fn run_with_progress(workers: usize, source: &str, signal: mpsc::Sender<()>) -> String {
    let mut vm = Vm::with_workers(workers).with_suspend_budget(64);
    vm.set_output_sink(OutputSink::new(move |_, text| {
        if text.contains("progress") {
            let _ = signal.send(());
        }
    }));
    optive::run_source_in_vm(&mut vm, source, "<blocking-io>")
        .unwrap()
        .display_string()
}

#[test]
fn http_body_wait_does_not_block_progress_on_one_or_many_workers() {
    for workers in [1, 4] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (send, recv) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut buf = [0; 2048];
            let _ = stream.read(&mut buf).unwrap();
            // Send headers first; it is the delayed response body that must not block.
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n")
                .unwrap();
            let advanced = recv.recv_timeout(Duration::from_secs(3)).is_ok();
            stream
                .write_all(if advanced { b"Y" } else { b"N" })
                .unwrap();
            advanced
        });
        let source = format!(
            r#"
let io = go std.http.get("http://{addr}/", {{"timeout": 5}})
let other = go do {{ print("progress")
    gc()
    return 42 }}
let response = await io
response["body"] == "Y" and (await other) == 42
"#
        );
        assert_eq!(run_with_progress(workers, &source, send), "true");
        assert!(server.join().unwrap());
    }
}

#[test]
fn tcp_read_wait_does_not_block_other_tasks() {
    for workers in [1, 4] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (send, recv) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let advanced = recv.recv_timeout(Duration::from_secs(3)).is_ok();
            stream
                .write_all(if advanced { b"Y" } else { b"N" })
                .unwrap();
            advanced
        });
        let source = format!(
            r#"
let conn = std.net.connect("127.0.0.1", {port})
let io = go conn.read(*[1])
let other = go do {{ print("progress")
    gc()
    return 42 }}
let data = await io
conn.close()
std.text.from_bytes(data) == "Y" and (await other) == 42
"#
        );
        assert_eq!(run_with_progress(workers, &source, send), "true");
        assert!(server.join().unwrap());
    }
}

#[test]
fn udp_receive_and_send_can_share_one_socket_on_one_worker() {
    let host = UdpSocket::bind("127.0.0.1:0").unwrap();
    host.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let port = host.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let mut data = [0; 16];
        let (_, from) = host.recv_from(&mut data).unwrap();
        host.send_to(b"42", from).unwrap();
    });
    let source = format!(
        r#"
let sock = std.net.bind_udp("127.0.0.1", 0)
let read = go sock.recv_from()
let write = go sock.send_to("hi", "127.0.0.1", {port})
let result = await read
await write
sock.close()
std.text.from_bytes(result["data"]) == "42"
"#
    );
    let (send, _) = mpsc::channel();
    assert_eq!(run_with_progress(1, &source, send), "true");
    server.join().unwrap();
}

#[test]
fn process_wait_and_main_wait_both_drive_the_scheduler() {
    let command = if cfg!(windows) {
        r#"["cmd", "/c", "ping -n 2 127.0.0.1 >nul & echo 42"]"#
    } else {
        r#"["sh", "-c", "sleep 0.1; printf 42"]"#
    };
    for workers in [1, 4] {
        for asynchronous in [false, true] {
            let (send, recv) = mpsc::channel();
            let source = if asynchronous {
                format!(
                    r#"
let slow = go std.os.capture({command})
let fast = go do {{ print("progress")
    return not slow.done() }}
let advanced = await fast
let result = await slow
advanced and result["status"] == 0
"#
                )
            } else {
                format!(
                    r#"
let fast = go do {{ print("progress")
    return 42 }}
let result = std.os.capture({command})
fast.done() and (await fast) == 42 and result["status"] == 0
"#
                )
            };
            assert_eq!(run_with_progress(workers, &source, send), "true");
            recv.try_recv().unwrap();
        }
    }
}

#[test]
fn native_errors_preserve_exception_kind_after_resumption() {
    for workers in [1, 4] {
        let source = r#"
let task = go std.os.capture(["optive_nonexistent_executable_123456"])
var caught = false
try { await task } catch (e: IOError) { caught = true }
caught and task.done()
"#;
        let (send, _) = mpsc::channel();
        assert_eq!(run_with_progress(workers, source, send), "true");
    }
}

#[test]
fn input_wait_allows_progress_and_prints_prompt_only_once() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    for workers in [1, 4] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_Optive"))
            .env("OPTIVE_WORKERS", workers.to_string())
            .args([
                "--color=never",
                "-c",
                r#"
let input_task = go input("prompt:")
let other = go do { print("progress")
    return 42 }
let answer = await input_task
print(answer, await other)
"#,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, recv) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut output = String::new();
            for line in BufReader::new(stdout).lines() {
                let line = line.unwrap();
                output.push_str(&line);
                output.push('\n');
                if line.contains("progress") {
                    let _ = send.send(());
                }
            }
            output
        });
        if recv.recv_timeout(Duration::from_secs(10)).is_err() {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "input blocked progress: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"hello\n").unwrap();
        drop(stdin);
        let status = child.wait_with_output().unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        let output = reader.join().unwrap();
        assert_eq!(output.matches("prompt:").count(), 1, "{output}");
        assert!(output.contains("hello 42"), "{output}");
    }
}

#[test]
fn offloading_does_not_bypass_network_process_or_file_capabilities() {
    for source in [
        r#"await std.http.get("http://127.0.0.1:1/")"#,
        r#"await std.net.connect("127.0.0.1", 1)"#,
        r#"await std.os.capture(["cmd"] )"#,
        r#"await std.io.read_file("../outside.txt")"#,
    ] {
        let mut vm = Vm::with_workers(1);
        vm.caps = optive::caps::Capabilities::sandbox(vec![]);
        let error = optive::run_source_in_vm(&mut vm, source, "<io-caps>").unwrap_err();
        assert!(error.to_string().contains("sandbox"), "{error}");
    }
}
