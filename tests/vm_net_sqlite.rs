#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro
)]
//! `std.net` / `std.sqlite` 冒烟。

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use common::{run_with_caps, value};
use optive::caps::Capabilities;
use optive::run_source;
use optive::value::Value;

fn tmp(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "optive_{name}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    p
}

#[test]
fn sqlite_create_insert_query() {
    let p = tmp("sqlite.db");
    let path = p.to_string_lossy().replace('\\', "/");
    let src = format!(
        r#"
let db = std.sqlite.open("{path}")
db.execute("CREATE TABLE t (id INTEGER, name TEXT)")
db.execute("INSERT INTO t VALUES (?, ?)", [1, "ada"])
let rows = db.query("SELECT id, name FROM t")
db.close()
rows[0]["name"]
"#
    );
    let v = value(&src);
    match v {
        Value::Text(s) => assert_eq!(s, "ada"),
        other => panic!("{}", other.display_string()),
    }
    let _ = std::fs::remove_file(&p);
}

#[test]
fn sqlite_memory_roundtrip() {
    let v = value(
        r#"
let db = std.sqlite.open(":memory:")
db.execute("CREATE TABLE t (n INTEGER)")
db.execute("INSERT INTO t VALUES (7)")
let rows = db.query("SELECT n FROM t")
rows[0]["n"]
"#,
    );
    match v {
        Value::Num(n) => assert_eq!(n.to_i64(), Some(7)),
        other => panic!("{}", other.display_string()),
    }
}

#[test]
fn sqlite_sandbox_blocks_file() {
    let caps = Capabilities::sandbox(vec![std::env::current_dir().unwrap()]);
    let err = run_with_caps(r#"std.sqlite.open("../escape.db")"#, caps).expect_err("sandbox");
    let msg = err.to_string();
    assert!(
        msg.contains("sandbox") || msg.contains("outside") || msg.contains("disabled"),
        "{msg}"
    );
}

#[test]
fn net_sandbox_blocks_listen() {
    let caps = Capabilities::sandbox(vec![std::env::current_dir().unwrap()]);
    let err = run_with_caps(r#"std.net.listen(0)"#, caps).expect_err("sandbox");
    let msg = err.to_string();
    assert!(msg.contains("network") || msg.contains("sandbox"), "{msg}");
}

#[test]
fn net_listen_accept_echo() {
    let addr_file = tmp("net_addr.txt");
    let addr_path = addr_file.to_string_lossy().replace('\\', "/");
    let src = format!(
        r#"
let ln = std.net.listen("127.0.0.1", 0)
std.fs.write_text("{addr_path}", ln.addr())
let c = ln.accept()
let b = c.read(16)
c.close()
ln.close()
std.text.from_bytes(b)
"#
    );

    let server = thread::spawn(move || value(&src));

    let deadline = Instant::now() + Duration::from_secs(5);
    let addr = loop {
        if Instant::now() > deadline {
            panic!("listener did not write addr file");
        }
        if let Ok(s) = std::fs::read_to_string(&addr_file) {
            let t = s.trim();
            if !t.is_empty() {
                break t.to_string();
            }
        }
        thread::sleep(Duration::from_millis(10));
    };

    let mut stream = TcpStream::connect(&addr).expect("connect");
    stream.write_all(b"hello").expect("write");
    drop(stream);

    let v = server.join().expect("server thread");
    match v {
        Value::Text(s) => assert_eq!(s, "hello"),
        other => panic!("{}", other.display_string()),
    }
    let _ = std::fs::remove_file(&addr_file);
}

#[test]
fn http_serve_echo_loopback() {
    let port = 21000u16 + (std::process::id() % 1000) as u16;
    let err_slot = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let err_w = err_slot.clone();
    let src = format!(
        r#"
func on_req(req) {{
  return {{ "status": 200, "body": "pong" }}
}}
std.http.serve("127.0.0.1", {port}, on_req)
"#
    );
    let _server = thread::spawn(move || {
        if let Err(e) = run_source(&src) {
            *err_w.lock().unwrap() = Some(e.to_string());
        }
    });

    let url_host = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut stream = loop {
        if Instant::now() > deadline {
            let err = err_slot.lock().unwrap().clone();
            panic!("http.serve did not accept in time: {err:?}");
        }
        if let Some(e) = err_slot.lock().unwrap().as_ref() {
            panic!("http.serve failed: {e}");
        }
        match TcpStream::connect(&url_host) {
            Ok(s) => break s,
            Err(_) => thread::sleep(Duration::from_millis(20)),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .expect("timeout");
    stream
        .write_all(b"GET /hello HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .expect("write");
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).expect("read");
    let text = String::from_utf8_lossy(&buf);
    assert!(text.contains("pong"), "{text}");

    let url = format!("http://{url_host}/");
    match run_source(&format!(
        r#"std.http.get("{url}", {{ "timeout": 5 }})["body"]"#
    )) {
        Ok(Value::Text(s)) => assert!(s.contains("pong"), "{s}"),
        Ok(other) => panic!("{}", other.display_string()),
        Err(e) => panic!("std.http.get loopback: {e}"),
    }
}

#[test]
fn http_serve_sandbox_blocked() {
    let caps = Capabilities::sandbox(vec![std::env::current_dir().unwrap()]);
    let err = run_with_caps(
        r#"
func h(req) { return { "body": "x" } }
std.http.serve(1, h)
"#,
        caps,
    )
    .expect_err("sandbox");
    let msg = err.to_string();
    assert!(msg.contains("network") || msg.contains("sandbox"), "{msg}");
}

#[test]
fn net_connect_tls_sandbox_blocked() {
    let caps = Capabilities::sandbox(vec![std::env::current_dir().unwrap()]);
    let err =
        run_with_caps(r#"std.net.connect_tls("example.com", 443)"#, caps).expect_err("sandbox");
    let msg = err.to_string();
    assert!(msg.contains("network") || msg.contains("sandbox"), "{msg}");
}

#[test]
#[ignore = "hits the public internet; set OPTIVE_NET_TEST=1 and run with --ignored"]
fn net_connect_tls_example_com() {
    if std::env::var("OPTIVE_NET_TEST").ok().as_deref() != Some("1") {
        return;
    }
    let v = value(
        r#"
let c = std.net.connect_tls("example.com", 443)
c.write("GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n")
let b = c.read(64)
c.close()
std.text.from_bytes(b)
"#,
    );
    match v {
        Value::Text(s) => {
            assert!(s.contains("HTTP/"), "{s}");
        }
        other => panic!("{}", other.display_string()),
    }
}

#[test]
fn net_udp_loopback() {
    let v = value(
        r#"
let a = std.net.bind_udp("127.0.0.1", 0)
let b = std.net.bind_udp("127.0.0.1", 0)
let parts = std.text.split(b.addr(), ":")
let port = int(parts[len(parts) - 1])
a.send_to("ping", "127.0.0.1", port)
let msg = b.recv_from()
a.close()
b.close()
std.text.from_bytes(msg["data"])
"#,
    );
    match v {
        Value::Text(s) => assert_eq!(s, "ping"),
        other => panic!("{}", other.display_string()),
    }
}

#[test]
fn net_ws_echo() {
    let addr_file = tmp("ws_addr.txt");
    let addr_path = addr_file.to_string_lossy().replace('\\', "/");
    let src = format!(
        r#"
let ln = std.net.listen("127.0.0.1", 0)
std.fs.write_text("{addr_path}", ln.addr())
let c = ln.accept()
let w = std.net.ws_accept(c)
let m = w.recv()
w.close()
ln.close()
m
"#
    );
    let server = thread::spawn(move || value(&src));
    let deadline = Instant::now() + Duration::from_secs(5);
    let addr = loop {
        if Instant::now() > deadline {
            panic!("ws listener did not write addr");
        }
        if let Ok(s) = std::fs::read_to_string(&addr_file) {
            let t = s.trim();
            if !t.is_empty() {
                break t.to_string();
            }
        }
        thread::sleep(Duration::from_millis(10));
    };
    let port: i64 = addr
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .expect("port");
    let client = format!(
        r#"
let w = std.net.ws_connect("127.0.0.1", {port})
w.send("hello-ws")
w.close()
0
"#
    );
    let _ = value(&client);
    let v = server.join().expect("server");
    match v {
        Value::Text(s) => assert_eq!(s, "hello-ws"),
        other => panic!("{}", other.display_string()),
    }
    let _ = std::fs::remove_file(&addr_file);
}

#[test]
fn net_listen_tls_loopback() {
    let dir = tmp("tls_certs");
    let _ = std::fs::create_dir_all(&dir);
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    std::fs::write(&cert_path, certified.cert.pem()).unwrap();
    std::fs::write(&key_path, certified.key_pair.serialize_pem()).unwrap();
    let cert_s = cert_path.to_string_lossy().replace('\\', "/");
    let key_s = key_path.to_string_lossy().replace('\\', "/");
    let addr_file = tmp("tls_addr.txt");
    let addr_path = addr_file.to_string_lossy().replace('\\', "/");
    let src = format!(
        r#"
let ln = std.net.listen_tls("127.0.0.1", 0, "{cert_s}", "{key_s}")
std.fs.write_text("{addr_path}", ln.addr())
let c = ln.accept()
let b = c.read(16)
c.close()
ln.close()
std.text.from_bytes(b)
"#
    );
    let server = thread::spawn(move || value(&src));
    let deadline = Instant::now() + Duration::from_secs(8);
    let addr = loop {
        if Instant::now() > deadline {
            panic!("tls listener did not write addr");
        }
        if let Ok(s) = std::fs::read_to_string(&addr_file) {
            let t = s.trim();
            if !t.is_empty() {
                break t.to_string();
            }
        }
        thread::sleep(Duration::from_millis(10));
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(certified.cert.der().clone())
        .expect("add test ca");
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let tcp = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if Instant::now() > deadline {
                panic!("tls connect timeout");
            }
            match TcpStream::connect(&addr) {
                Ok(s) => break s,
                Err(_) => thread::sleep(Duration::from_millis(20)),
            }
        }
    };
    let _ = tcp.set_nodelay(true);
    let conn = rustls::ClientConnection::new(std::sync::Arc::new(cfg), name).unwrap();
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    tls.write_all(b"hello-tls").unwrap();
    tls.flush().unwrap();
    let v = server.join().expect("server");
    tls.conn.send_close_notify();
    let _ = tls.flush();
    match v {
        Value::Text(s) => assert_eq!(s, "hello-tls"),
        other => panic!("{}", other.display_string()),
    }
    let _ = std::fs::remove_file(&addr_file);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn http_serve_tls_loopback() {
    let dir = tmp("https_certs");
    let _ = std::fs::create_dir_all(&dir);
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    let certified =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    std::fs::write(&cert_path, certified.cert.pem()).unwrap();
    std::fs::write(&key_path, certified.key_pair.serialize_pem()).unwrap();
    let cert_s = cert_path.to_string_lossy().replace('\\', "/");
    let key_s = key_path.to_string_lossy().replace('\\', "/");
    let port = 22000u16 + (std::process::id() % 900) as u16;
    let err_slot = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let err_w = err_slot.clone();
    let src = format!(
        r#"
func on_req(req) {{
  return {{ "status": 200, "body": "pong-tls" }}
}}
std.http.serve_tls("127.0.0.1", {port}, on_req, "{cert_s}", "{key_s}")
"#
    );
    let _server = thread::spawn(move || {
        if let Err(e) = run_source(&src) {
            *err_w.lock().unwrap() = Some(e.to_string());
        }
    });
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(certified.cert.der().clone())
        .expect("add test ca");
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let tcp = loop {
        if Instant::now() > deadline {
            let err = err_slot.lock().unwrap().clone();
            panic!("serve_tls did not accept: {err:?}");
        }
        if let Some(e) = err_slot.lock().unwrap().as_ref() {
            panic!("serve_tls failed: {e}");
        }
        match TcpStream::connect(format!("127.0.0.1:{port}")) {
            Ok(s) => break s,
            Err(_) => thread::sleep(Duration::from_millis(20)),
        }
    };
    let _ = tcp.set_nodelay(true);
    let conn = rustls::ClientConnection::new(std::sync::Arc::new(cfg), name).unwrap();
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    tls.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    tls.flush().unwrap();
    let mut buf = Vec::new();
    match tls.read_to_end(&mut buf) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !buf.is_empty() => {
            // rustls treats a TCP close without close_notify as UnexpectedEof;
            // the response body may already be in `buf`.
        }
        Err(e) => panic!("https read: {e}"),
    }
    let text = String::from_utf8_lossy(&buf);
    assert!(text.contains("pong-tls"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hash_sha512_known() {
    let v = value(r#"std.hash.sha512("hi")"#);
    match v {
        Value::Text(s) => assert_eq!(s.len(), 128),
        other => panic!("{}", other.display_string()),
    }
}
