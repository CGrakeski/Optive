//! `std.net`：阻塞 TCP / TLS / UDP / WebSocket。

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use parking_lot::Mutex;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

use crate::error::RuntimeError;
use crate::shared::Shared;
use crate::value::{DictMap, ModuleObject, Num, Value, ValueKey};
use crate::vm::Vm;
use crate::Result;

use super::{
    expect_arity, expect_int, expect_text, expect_text_or_bytes, exports, io_map, named_builtin,
    submodule,
};

type ConnSlot = Arc<Mutex<Option<ConnInner>>>;

fn conn_slots() -> &'static Mutex<HashMap<usize, ConnSlot>> {
    static SLOTS: OnceLock<Mutex<HashMap<usize, ConnSlot>>> = OnceLock::new();
    SLOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

struct ConnRegistration {
    key: AtomicUsize,
    slot: Weak<Mutex<Option<ConnInner>>>,
}

impl ConnRegistration {
    fn new(slot: &ConnSlot) -> Self {
        Self {
            key: AtomicUsize::new(0),
            slot: Arc::downgrade(slot),
        }
    }

    fn set_key(&self, key: usize) {
        self.key.store(key, Ordering::Release);
    }

    fn remove(&self) {
        let key = self.key.load(Ordering::Acquire);
        if key != 0 {
            let Some(slot) = self.slot.upgrade() else {
                return;
            };
            let mut slots = conn_slots().lock();
            if slots
                .get(&key)
                .is_some_and(|registered| Arc::ptr_eq(registered, &slot))
            {
                slots.remove(&key);
            }
        }
    }
}

impl Drop for ConnRegistration {
    fn drop(&mut self) {
        self.remove();
    }
}

pub(super) fn build_net_module() -> Shared<ModuleObject> {
    submodule(
        "net",
        &[
            ("listen", named_builtin("listen", net_listen)),
            ("listen_tls", named_builtin("listen_tls", net_listen_tls)),
            ("connect", named_builtin("connect", net_connect)),
            ("connect_tls", named_builtin("connect_tls", net_connect_tls)),
            ("bind_udp", named_builtin("bind_udp", net_bind_udp)),
            ("ws_connect", named_builtin("ws_connect", net_ws_connect)),
            (
                "ws_connect_tls",
                named_builtin("ws_connect_tls", net_ws_connect_tls),
            ),
            ("ws_accept", named_builtin("ws_accept", net_ws_accept)),
        ],
    )
}

fn check_port(name: &str, port: i64) -> Result<u16> {
    if !(0..=65535).contains(&port) {
        return Err(RuntimeError::value_err(format!(
            "{name}: port {port} out of range"
        )));
    }
    Ok(port as u16)
}

fn net_listen(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("listen")?;
    let (host, port) = match args.len() {
        1 => ("127.0.0.1".to_string(), expect_int("listen", args, 0)?),
        2 => (
            expect_text("listen", args, 0)?,
            expect_int("listen", args, 1)?,
        ),
        _ => {
            return Err(RuntimeError::type_err(
                "listen requires (port) or (host, port)",
            ));
        }
    };
    let port = check_port("listen", port)?;
    let bind = format!("{host}:{port}");
    let listener = TcpListener::bind(&bind).map_err(|e| io_map(&format!("listen {bind}"), e))?;
    Ok(wrap_listener(listener, None))
}

fn net_listen_tls(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("listen_tls")?;
    let (host, port, cert, key) = match args.len() {
        3 => (
            "127.0.0.1".to_string(),
            expect_int("listen_tls", args, 0)?,
            expect_text("listen_tls", args, 1)?,
            expect_text("listen_tls", args, 2)?,
        ),
        4 => (
            expect_text("listen_tls", args, 0)?,
            expect_int("listen_tls", args, 1)?,
            expect_text("listen_tls", args, 2)?,
            expect_text("listen_tls", args, 3)?,
        ),
        _ => {
            return Err(RuntimeError::type_err(
                "listen_tls requires (port, cert, key) or (host, port, cert, key)",
            ));
        }
    };
    let cert_file = vm.caps.open_read("listen_tls certificate", &cert)?;
    let key_file = vm.caps.open_read("listen_tls private key", &key)?;
    let port = check_port("listen_tls", port)?;
    let config = load_server_config(cert_file, key_file)?;
    let bind = format!("{host}:{port}");
    let listener =
        TcpListener::bind(&bind).map_err(|e| io_map(&format!("listen_tls {bind}"), e))?;
    Ok(wrap_listener(listener, Some(config)))
}

fn net_connect(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("connect")?;
    expect_arity("connect", args, 2)?;
    let host = expect_text("connect", args, 0)?;
    let port = check_port("connect", expect_int("connect", args, 1)?)?;
    let addr = format!("{host}:{port}");
    let Some(stream) = vm.run_blocking(move || {
        TcpStream::connect(&addr).map_err(|e| io_map(&format!("connect {addr}"), e))
    })?
    else {
        return Ok(Value::None);
    };
    let _ = stream.set_nodelay(true);
    Ok(wrap_conn(ConnInner::Plain(stream)))
}

fn net_connect_tls(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("connect_tls")?;
    expect_arity("connect_tls", args, 2)?;
    let host = expect_text("connect_tls", args, 0)?;
    let port = check_port("connect_tls", expect_int("connect_tls", args, 1)?)?;
    let addr = format!("{host}:{port}");
    let Some(tls) = vm.run_blocking(move || {
        let tcp =
            TcpStream::connect(&addr).map_err(|e| io_map(&format!("connect_tls {addr}"), e))?;
        let _ = tcp.set_nodelay(true);
        tls_wrap(tcp, &host)
    })?
    else {
        return Ok(Value::None);
    };
    Ok(wrap_conn(ConnInner::Tls(Box::new(tls))))
}

fn net_bind_udp(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("bind_udp")?;
    let (host, port) = match args.len() {
        1 => ("127.0.0.1".to_string(), expect_int("bind_udp", args, 0)?),
        2 => (
            expect_text("bind_udp", args, 0)?,
            expect_int("bind_udp", args, 1)?,
        ),
        _ => {
            return Err(RuntimeError::type_err(
                "bind_udp requires (port) or (host, port)",
            ));
        }
    };
    let port = check_port("bind_udp", port)?;
    let bind = format!("{host}:{port}");
    let sock = UdpSocket::bind(&bind).map_err(|e| io_map(&format!("bind_udp {bind}"), e))?;
    Ok(wrap_udp(sock))
}

fn ws_path(args: &[Value], name: &str) -> Result<String> {
    match args.len() {
        2 => Ok("/".into()),
        3 => {
            let p = expect_text(name, args, 2)?;
            if p.starts_with('/') {
                Ok(p)
            } else {
                Ok(format!("/{p}"))
            }
        }
        _ => Err(RuntimeError::type_err(format!(
            "{name} requires (host, port) or (host, port, path)"
        ))),
    }
}

fn net_ws_connect(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("ws_connect")?;
    let host = expect_text("ws_connect", args, 0)?;
    let port = check_port("ws_connect", expect_int("ws_connect", args, 1)?)?;
    let path = ws_path(args, "ws_connect")?;
    let addr = format!("{host}:{port}");
    let uri = format!("ws://{host}:{port}{path}");
    let Some(ws) = vm.run_blocking(move || {
        let tcp =
            TcpStream::connect(&addr).map_err(|e| io_map(&format!("ws_connect {addr}"), e))?;
        let _ = tcp.set_nodelay(true);
        handshake_client(&uri, ConnInner::Plain(tcp))
    })?
    else {
        return Ok(Value::None);
    };
    Ok(wrap_ws(ws))
}

fn net_ws_connect_tls(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("ws_connect_tls")?;
    let host = expect_text("ws_connect_tls", args, 0)?;
    let port = check_port("ws_connect_tls", expect_int("ws_connect_tls", args, 1)?)?;
    let path = ws_path(args, "ws_connect_tls")?;
    let addr = format!("{host}:{port}");
    let uri = format!("wss://{host}:{port}{path}");
    let Some(ws) = vm.run_blocking(move || {
        let tcp =
            TcpStream::connect(&addr).map_err(|e| io_map(&format!("ws_connect_tls {addr}"), e))?;
        let _ = tcp.set_nodelay(true);
        let tls = tls_wrap(tcp, &host)?;
        handshake_client(&uri, ConnInner::Tls(Box::new(tls)))
    })?
    else {
        return Ok(Value::None);
    };
    Ok(wrap_ws(ws))
}

fn handshake_client(uri: &str, stream: ConnInner) -> Result<tungstenite::WebSocket<ConnInner>> {
    let (ws, _) = tungstenite::client(uri, stream).map_err(|e| io_map("ws_connect", e))?;
    Ok(ws)
}

fn net_ws_accept(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("ws_accept")?;
    expect_arity("ws_accept", args, 1)?;
    let key = conn_key(&args[0])?;
    let Some(ws) = vm.run_blocking(move || {
        let stream = take_conn(key)?;
        tungstenite::accept(stream).map_err(|e| io_map("ws_accept", e))
    })?
    else {
        return Ok(Value::None);
    };
    Ok(wrap_ws(ws))
}

fn ensure_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn tls_wrap(
    tcp: TcpStream,
    host: &str,
) -> Result<rustls::StreamOwned<rustls::ClientConnection, TcpStream>> {
    ensure_crypto();
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for cert in native.certs {
        let _ = roots.add(cert);
    }
    if roots.is_empty() {
        return Err(RuntimeError::io_err(
            "connect_tls: no native root certificates loaded",
        ));
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| RuntimeError::value_err(format!("connect_tls: invalid host {host}: {e}")))?;
    let conn = rustls::ClientConnection::new(Arc::new(config), server_name)
        .map_err(|e| io_map("connect_tls", e))?;
    Ok(rustls::StreamOwned::new(conn, tcp))
}

pub(super) fn load_server_config(cert_file: File, key_file: File) -> Result<Arc<ServerConfig>> {
    ensure_crypto();
    let certs = load_certs(cert_file)?;
    let key = load_key(key_file)?;
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| io_map("tls cert", e))?;
    Ok(Arc::new(config))
}

fn load_certs(file: File) -> Result<Vec<CertificateDer<'static>>> {
    CertificateDer::pem_reader_iter(BufReader::new(file))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| io_map("tls cert", e))
}

fn load_key(file: File) -> Result<PrivateKeyDer<'static>> {
    PrivateKeyDer::from_pem_reader(BufReader::new(file)).map_err(|e| io_map("tls key", e))
}

pub(super) fn accept_tls(tcp: TcpStream, config: &Arc<ServerConfig>) -> Result<ConnInner> {
    let _ = tcp.set_nodelay(true);
    let mut conn = ServerConnection::new(config.clone()).map_err(|e| io_map("tls accept", e))?;
    let mut tcp = tcp;
    while conn.is_handshaking() {
        conn.complete_io(&mut tcp)
            .map_err(|e| io_map("tls handshake", e))?;
    }
    Ok(ConnInner::TlsServer(Box::new(StreamOwned::new(conn, tcp))))
}

pub(super) enum ConnInner {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
    TlsServer(Box<StreamOwned<ServerConnection, TcpStream>>),
}

impl ConnInner {
    fn peer_addr(&self) -> io::Result<std::net::SocketAddr> {
        match self {
            Self::Plain(s) => s.peer_addr(),
            Self::Tls(s) => s.sock.peer_addr(),
            Self::TlsServer(s) => s.sock.peer_addr(),
        }
    }

    pub(super) fn shutdown_tls(&mut self) {
        match self {
            Self::Plain(s) => {
                let _ = s.shutdown(std::net::Shutdown::Write);
            }
            Self::Tls(s) => {
                s.conn.send_close_notify();
                let _ = s.flush();
            }
            Self::TlsServer(s) => {
                s.conn.send_close_notify();
                let _ = s.flush();
            }
        }
    }
}

impl Read for ConnInner {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(buf),
            Self::Tls(s) => s.read(buf),
            Self::TlsServer(s) => s.read(buf),
        }
    }
}

impl Write for ConnInner {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(buf),
            Self::Tls(s) => s.write(buf),
            Self::TlsServer(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(s) => s.flush(),
            Self::TlsServer(s) => s.flush(),
        }
    }
}

fn wrap_listener(listener: TcpListener, tls: Option<Arc<ServerConfig>>) -> Value {
    let inner = Arc::new(Mutex::new(Some(listener)));
    let accept_h = inner.clone();
    let addr_h = inner.clone();
    let close_h = inner;
    let tls_accept = tls.clone();
    Value::Module(Shared::new(ModuleObject {
        name: "Listener".into(),
        exports: exports(&[
            (
                "accept",
                Value::builtin("accept", move |vm, _| {
                    vm.caps.check_network("accept")?;
                    let handle = accept_h.clone();
                    let tls = tls_accept.clone();
                    let Some(conn) = vm.run_blocking(move || {
                        let listener = handle
                            .lock()
                            .as_ref()
                            .ok_or_else(|| RuntimeError::io_err("accept: listener is closed"))?
                            .try_clone()
                            .map_err(|e| io_map("accept", e))?;
                        let (stream, _) = listener.accept().map_err(|e| io_map("accept", e))?;
                        let _ = stream.set_nodelay(true);
                        if let Some(cfg) = tls {
                            accept_tls(stream, &cfg)
                        } else {
                            Ok(ConnInner::Plain(stream))
                        }
                    })?
                    else {
                        return Ok(Value::None);
                    };
                    Ok(wrap_conn(conn))
                }),
            ),
            (
                "addr",
                Value::builtin("addr", move |_vm, _| {
                    let guard = addr_h.lock();
                    let ln = guard
                        .as_ref()
                        .ok_or_else(|| RuntimeError::io_err("addr: listener is closed"))?;
                    let a = ln.local_addr().map_err(|e| io_map("addr", e))?;
                    Ok(Value::Text(a.to_string()))
                }),
            ),
            (
                "close",
                Value::builtin("close", move |_vm, _| {
                    let _ = close_h.lock().take();
                    Ok(Value::None)
                }),
            ),
        ]),
        children: HashMap::new(),
        is_user: false,
        live_globals: None,
        ..Default::default()
    }))
}

fn wrap_conn(stream: ConnInner) -> Value {
    let inner = Arc::new(Mutex::new(Some(stream)));
    let registration = Arc::new(ConnRegistration::new(&inner));
    let read_h = inner.clone();
    let write_h = inner.clone();
    let peer_h = inner.clone();
    let close_h = inner.clone();
    let close_registration = registration.clone();
    let module = Shared::new(ModuleObject {
        name: "Conn".into(),
        exports: exports(&[
            (
                "read",
                Value::builtin("read", move |vm, args| {
                    vm.caps.check_network("read")?;
                    let n = match args.len() {
                        0 => 4096i64,
                        1 => expect_int("read", args, 0)?,
                        _ => {
                            return Err(RuntimeError::type_err(
                                "read requires 0 or 1 argument (max bytes)",
                            ));
                        }
                    };
                    if n <= 0 {
                        return Err(RuntimeError::value_err("read: max bytes must be > 0"));
                    }
                    let cap = usize::try_from(n)
                        .unwrap_or(usize::MAX)
                        .min(8 * 1024 * 1024);
                    let handle = read_h.clone();
                    let Some(buf) = vm.run_blocking(move || {
                        let mut buf = vec![0u8; cap];
                        let mut guard = handle.lock();
                        let s = guard
                            .as_mut()
                            .ok_or_else(|| RuntimeError::io_err("read: connection is closed"))?;
                        let got = if let ConnInner::Plain(tcp) = s {
                            let mut tcp = tcp.try_clone().map_err(|e| io_map("read", e))?;
                            drop(guard);
                            tcp.read(&mut buf).map_err(|e| io_map("read", e))?
                        } else {
                            s.read(&mut buf).map_err(|e| io_map("read", e))?
                        };
                        buf.truncate(got);
                        Ok(buf)
                    })?
                    else {
                        return Ok(Value::None);
                    };
                    Ok(Value::Bytes(Arc::new(buf)))
                }),
            ),
            (
                "write",
                Value::builtin("write", move |vm, args| {
                    vm.caps.check_network("write")?;
                    expect_arity("write", args, 1)?;
                    let bytes = expect_text_or_bytes("write", args, 0)?;
                    let handle = write_h.clone();
                    let Some(n) = vm.run_blocking(move || {
                        let mut guard = handle.lock();
                        let s = guard
                            .as_mut()
                            .ok_or_else(|| RuntimeError::io_err("write: connection is closed"))?;
                        if let ConnInner::Plain(tcp) = s {
                            let mut tcp = tcp.try_clone().map_err(|e| io_map("write", e))?;
                            drop(guard);
                            tcp.write_all(&bytes).map_err(|e| io_map("write", e))?;
                            tcp.flush().map_err(|e| io_map("write", e))?;
                        } else {
                            s.write_all(&bytes).map_err(|e| io_map("write", e))?;
                            s.flush().map_err(|e| io_map("write", e))?;
                        }
                        Ok(bytes.len())
                    })?
                    else {
                        return Ok(Value::None);
                    };
                    Ok(Value::Num(Num::Small(n as i64)))
                }),
            ),
            (
                "peer",
                Value::builtin("peer", move |vm, _| {
                    let handle = peer_h.clone();
                    let Some(addr) = vm.run_blocking(move || {
                        let guard = handle.lock();
                        let s = guard
                            .as_ref()
                            .ok_or_else(|| RuntimeError::io_err("peer: connection is closed"))?;
                        s.peer_addr().map_err(|e| io_map("peer", e))
                    })?
                    else {
                        return Ok(Value::None);
                    };
                    Ok(Value::Text(addr.to_string()))
                }),
            ),
            (
                "close",
                Value::builtin("close", move |vm, _| {
                    let registration = close_registration.clone();
                    let handle = close_h.clone();
                    if vm
                        .run_blocking(move || {
                            registration.remove();
                            if let Some(mut s) = handle.lock().take() {
                                s.shutdown_tls();
                            }
                            Ok(())
                        })?
                        .is_none()
                    {
                        return Ok(Value::None);
                    }
                    Ok(Value::None)
                }),
            ),
        ]),
        children: HashMap::new(),
        is_user: false,
        live_globals: None,
        ..Default::default()
    });
    let key = module.as_ptr() as usize;
    registration.set_key(key);
    conn_slots().lock().insert(key, inner);
    Value::Module(module)
}

fn conn_key(v: &Value) -> Result<usize> {
    let Value::Module(m) = v else {
        return Err(RuntimeError::type_err("ws_accept: argument must be a Conn"));
    };
    if m.borrow().name != "Conn" {
        return Err(RuntimeError::type_err("ws_accept: argument must be a Conn"));
    }
    Ok(m.as_ptr() as usize)
}

fn take_conn(key: usize) -> Result<ConnInner> {
    let slot = conn_slots()
        .lock()
        .remove(&key)
        .ok_or_else(|| RuntimeError::io_err("ws_accept: connection is closed or not detachable"))?;
    let taken = slot.lock().take();
    taken.ok_or_else(|| RuntimeError::io_err("ws_accept: connection is closed"))
}

fn wrap_udp(sock: UdpSocket) -> Value {
    let inner = Arc::new(Mutex::new(Some(sock)));
    let send_h = inner.clone();
    let recv_h = inner.clone();
    let addr_h = inner.clone();
    let close_h = inner;
    Value::Module(Shared::new(ModuleObject {
        name: "UdpSock".into(),
        exports: exports(&[
            (
                "send_to",
                Value::builtin("send_to", move |vm, args| {
                    vm.caps.check_network("send_to")?;
                    expect_arity("send_to", args, 3)?;
                    let bytes = match &args[0] {
                        Value::Text(s) => s.as_bytes().to_vec(),
                        Value::Bytes(b) => b.as_ref().clone(),
                        _ => {
                            return Err(RuntimeError::type_err(
                                "send_to: data must be text or bytes",
                            ));
                        }
                    };
                    let host = expect_text("send_to", args, 1)?;
                    let port = check_port("send_to", expect_int("send_to", args, 2)?)?;
                    let dest = format!("{host}:{port}");
                    let handle = send_h.clone();
                    let Some(n) = vm.run_blocking(move || {
                        let socket = handle
                            .lock()
                            .as_ref()
                            .ok_or_else(|| RuntimeError::io_err("send_to: socket is closed"))?
                            .try_clone()
                            .map_err(|e| io_map("send_to", e))?;
                        socket
                            .send_to(&bytes, &dest)
                            .map_err(|e| io_map("send_to", e))
                    })?
                    else {
                        return Ok(Value::None);
                    };
                    Ok(Value::Num(Num::Small(n as i64)))
                }),
            ),
            (
                "recv_from",
                Value::builtin("recv_from", move |vm, _| {
                    vm.caps.check_network("recv_from")?;
                    let handle = recv_h.clone();
                    let Some((buf, from)) = vm.run_blocking(move || {
                        let socket = handle
                            .lock()
                            .as_ref()
                            .ok_or_else(|| RuntimeError::io_err("recv_from: socket is closed"))?
                            .try_clone()
                            .map_err(|e| io_map("recv_from", e))?;
                        let mut buf = vec![0u8; 65535];
                        let (n, from) = socket
                            .recv_from(&mut buf)
                            .map_err(|e| io_map("recv_from", e))?;
                        buf.truncate(n);
                        Ok((buf, from))
                    })?
                    else {
                        return Ok(Value::None);
                    };
                    let mut d = DictMap::new();
                    d.insert(ValueKey::Text("data".into()), Value::Bytes(Arc::new(buf)));
                    d.insert(
                        ValueKey::Text("host".into()),
                        Value::Text(from.ip().to_string()),
                    );
                    d.insert(
                        ValueKey::Text("port".into()),
                        Value::Num(Num::Small(i64::from(from.port()))),
                    );
                    Ok(Value::Dict(Shared::new(d)))
                }),
            ),
            (
                "addr",
                Value::builtin("addr", move |_vm, _| {
                    let guard = addr_h.lock();
                    let s = guard
                        .as_ref()
                        .ok_or_else(|| RuntimeError::io_err("addr: socket is closed"))?;
                    let a = s.local_addr().map_err(|e| io_map("addr", e))?;
                    Ok(Value::Text(a.to_string()))
                }),
            ),
            (
                "close",
                Value::builtin("close", move |_vm, _| {
                    let _ = close_h.lock().take();
                    Ok(Value::None)
                }),
            ),
        ]),
        children: HashMap::new(),
        is_user: false,
        live_globals: None,
        ..Default::default()
    }))
}

fn wrap_ws(ws: tungstenite::WebSocket<ConnInner>) -> Value {
    let inner = Arc::new(Mutex::new(Some(ws)));
    let send_h = inner.clone();
    let recv_h = inner.clone();
    let close_h = inner;
    Value::Module(Shared::new(ModuleObject {
        name: "Ws".into(),
        exports: exports(&[
            (
                "send",
                Value::builtin("send", move |vm, args| {
                    vm.caps.check_network("ws_send")?;
                    expect_arity("send", args, 1)?;
                    let msg = match &args[0] {
                        Value::Text(s) => tungstenite::Message::Text(s.clone().into()),
                        Value::Bytes(b) => tungstenite::Message::Binary(b.as_ref().clone().into()),
                        _ => {
                            return Err(RuntimeError::type_err("send: data must be text or bytes"));
                        }
                    };
                    let handle = send_h.clone();
                    if vm
                        .run_blocking(move || {
                            let mut guard = handle.lock();
                            let ws = guard
                                .as_mut()
                                .ok_or_else(|| RuntimeError::io_err("send: websocket is closed"))?;
                            ws.send(msg).map_err(|e| io_map("send", e))
                        })?
                        .is_none()
                    {
                        return Ok(Value::None);
                    }
                    Ok(Value::None)
                }),
            ),
            (
                "recv",
                Value::builtin("recv", move |vm, _| {
                    vm.caps.check_network("ws_recv")?;
                    let handle = recv_h.clone();
                    let Some(msg) = vm.run_blocking(move || {
                        let mut guard = handle.lock();
                        let ws = guard
                            .as_mut()
                            .ok_or_else(|| RuntimeError::io_err("recv: websocket is closed"))?;
                        loop {
                            let msg = ws.read().map_err(|e| io_map("recv", e))?;
                            match msg {
                                tungstenite::Message::Text(_)
                                | tungstenite::Message::Binary(_)
                                | tungstenite::Message::Close(_) => return Ok(msg),
                                tungstenite::Message::Ping(p) => {
                                    let _ = ws.send(tungstenite::Message::Pong(p));
                                }
                                tungstenite::Message::Pong(_) | tungstenite::Message::Frame(_) => {}
                            }
                        }
                    })?
                    else {
                        return Ok(Value::None);
                    };
                    Ok(match msg {
                        tungstenite::Message::Text(t) => Value::Text(t.to_string()),
                        tungstenite::Message::Binary(b) => Value::Bytes(Arc::new(b.to_vec())),
                        _ => Value::None,
                    })
                }),
            ),
            (
                "close",
                Value::builtin("close", move |vm, _| {
                    let handle = close_h.clone();
                    if vm
                        .run_blocking(move || {
                            if let Some(mut ws) = handle.lock().take() {
                                let _ = ws.close(None);
                            }
                            Ok(())
                        })?
                        .is_none()
                    {
                        return Ok(Value::None);
                    }
                    Ok(Value::None)
                }),
            ),
        ]),
        children: HashMap::new(),
        is_user: false,
        live_globals: None,
        ..Default::default()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let client = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        (client, server)
    }

    #[test]
    fn conn_slots_do_not_grow_after_conn_lifetime() {
        let baseline = conn_slots().lock().len();
        for i in 0..32 {
            let (client, server) = tcp_pair();
            let conn = wrap_conn(ConnInner::Plain(server));
            assert_eq!(conn_slots().lock().len(), baseline + 1);
            if i % 2 == 0 {
                let Value::Module(module) = &conn else {
                    panic!("connection is not module");
                };
                let close = module.borrow().get_export("close").expect("close");
                let Value::Builtin(close) = close else {
                    panic!("close is not builtin");
                };
                close.call(&mut Vm::new(), &[]).expect("close");
                assert_eq!(conn_slots().lock().len(), baseline);
            }
            drop(conn);
            drop(client);
            assert_eq!(conn_slots().lock().len(), baseline);
        }
    }
}
