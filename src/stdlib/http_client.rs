use super::{expect_text, named_builtin, submodule};
use crate::shared::Shared;
use crate::value::{builtin_repr, DictMap, ModuleObject, Num, Value, ValueKey};
use crate::vm::Vm;
use crate::Result;

pub(super) fn build_http_module() -> Shared<ModuleObject> {
    submodule(
        "http",
        &[
            ("get", named_builtin("get", http_get)),
            ("post", named_builtin("post", http_post)),
            ("put", named_builtin("put", http_put)),
            ("delete", named_builtin("delete", http_delete)),
            ("patch", named_builtin("patch", http_patch)),
            ("head", named_builtin("head", http_head)),
            ("request", named_builtin("request", http_request)),
            (
                "serve",
                named_builtin("serve", super::http_server::http_serve),
            ),
            (
                "serve_tls",
                named_builtin("serve_tls", super::http_server::http_serve_tls),
            ),
        ],
    )
}

fn extract_headers(name: &str, opts: &Value) -> Result<reqwest::header::HeaderMap> {
    let mut headers = reqwest::header::HeaderMap::new();
    // 只读 opts.headers 子表；避免 timeout/proxy/body 等控制字段被误当作请求头。
    let hdrs = match opts {
        Value::Dict(d) => d.borrow().get(&ValueKey::Text("headers".into())).cloned(),
        _ => None,
    };
    if let Some(Value::Dict(d)) = hdrs {
        for (k, v) in d.borrow().iter() {
            let key_str = match k {
                ValueKey::Text(s) => s.as_str(),
                _ => continue,
            };
            let val_str = match v {
                Value::Text(s) => s.as_str(),
                Value::Num(n) => &n.to_string(),
                Value::Bool(b) => &b.to_string(),
                _ => continue,
            };
            let hn = reqwest::header::HeaderName::try_from(key_str).map_err(|e| {
                crate::error::RuntimeError::value_err(format!(
                    "{name}: invalid header name '{key_str}': {e}"
                ))
            })?;
            let hv = reqwest::header::HeaderValue::try_from(val_str).map_err(|e| {
                crate::error::RuntimeError::value_err(format!("{name}: invalid header value: {e}"))
            })?;
            headers.insert(hn, hv);
        }
    }
    Ok(headers)
}

fn opt_str(opts: &Value, key: &str) -> Option<String> {
    if let Value::Dict(d) = opts {
        if let Some(Value::Text(s)) = d.borrow().get(&ValueKey::Text(key.into())) {
            return Some(s.clone());
        }
    }
    None
}

fn opt_bool(opts: &Value, key: &str) -> Option<bool> {
    if let Value::Dict(d) = opts {
        if let Some(Value::Bool(b)) = d.borrow().get(&ValueKey::Text(key.into())) {
            return Some(*b);
        }
    }
    None
}

fn opt_num(opts: &Value, key: &str) -> Option<i64> {
    if let Value::Dict(d) = opts {
        if let Some(Value::Num(n)) = d.borrow().get(&ValueKey::Text(key.into())) {
            return n.to_i64();
        }
    }
    None
}

fn extract_timeout(opts: &Value) -> Option<std::time::Duration> {
    if let Value::Dict(d) = opts {
        if let Some(Value::Num(n)) = d.borrow().get(&ValueKey::Text("timeout".into())) {
            if let Some(secs) = n.to_i64() {
                return Some(std::time::Duration::from_secs(secs.max(0) as u64));
            }
        }
    }
    None
}

fn response_to_dict(resp: reqwest::blocking::Response) -> Result<Value> {
    let status = resp.status().as_u16();
    let url = resp.url().to_string();
    let mut header_map = DictMap::new();
    for (k, v) in resp.headers() {
        let val = v.to_str().unwrap_or("");
        header_map.insert(
            ValueKey::Text(k.as_str().to_string()),
            Value::Text(val.to_string()),
        );
    }
    let body = resp.text().map_err(|e| {
        crate::error::RuntimeError::io_err(format!("http: failed to read body: {e}"))
    })?;
    let mut out = DictMap::new();
    out.insert(
        ValueKey::Text("status".into()),
        Value::Num(Num::Small(i64::from(status))),
    );
    out.insert(ValueKey::Text("body".into()), Value::Text(body));
    out.insert(
        ValueKey::Text("headers".into()),
        Value::Dict(Shared::new(header_map)),
    );
    out.insert(
        ValueKey::Text("ok".into()),
        Value::Bool((200..300).contains(&status)),
    );
    out.insert(ValueKey::Text("url".into()), Value::Text(url));
    Ok(Value::Dict(Shared::new(out)))
}

fn build_client(opts: &Value) -> Result<reqwest::blocking::Client> {
    let mut builder = reqwest::blocking::Client::builder();
    if let Some(dur) = extract_timeout(opts) {
        builder = builder.timeout(dur);
    }
    if let Some(p) = opt_str(opts, "proxy") {
        let proxy = reqwest::Proxy::all(&p).map_err(|e| {
            crate::error::RuntimeError::value_err(format!("http: invalid proxy '{p}': {e}"))
        })?;
        builder = builder.proxy(proxy);
    }
    if let Some(ua) = opt_str(opts, "user_agent") {
        builder = builder.user_agent(ua);
    }
    if let Some(follow) = opt_bool(opts, "follow_redirects") {
        builder = builder.redirect(if follow {
            reqwest::redirect::Policy::default()
        } else {
            reqwest::redirect::Policy::none()
        });
    } else if let Some(n) = opt_num(opts, "follow_redirects") {
        builder = builder.redirect(reqwest::redirect::Policy::limited(n.max(0) as usize));
    }
    builder.build().map_err(|e| {
        crate::error::RuntimeError::io_err(format!("http: failed to build client: {e}"))
    })
}

fn apply_auth(
    mut req: reqwest::blocking::RequestBuilder,
    opts: &Value,
) -> reqwest::blocking::RequestBuilder {
    if let Some(auth) = opt_str(opts, "auth") {
        if let Some(idx) = auth.find(':') {
            let (u, p) = auth.split_at(idx);
            req = req.basic_auth(u, Some(&p[1..]));
        }
    } else if let Value::Dict(d) = opts {
        let auth_val = d.borrow().get(&ValueKey::Text("auth".into())).cloned();
        if let Some(Value::Dict(ad)) = auth_val {
            let user = match ad.borrow().get(&ValueKey::Text("user".into())) {
                Some(Value::Text(s)) => s.clone(),
                _ => String::new(),
            };
            let pass = match ad.borrow().get(&ValueKey::Text("pass".into())) {
                Some(Value::Text(s)) => Some(s.clone()),
                _ => None,
            };
            req = req.basic_auth(user, pass);
        }
    }
    req
}

fn send_request(
    vm: &mut Vm,
    op: &str,
    url: &str,
    opts: &Value,
    req: reqwest::blocking::RequestBuilder,
) -> Result<Value> {
    let mut req = req;
    if let Value::Dict(_) = opts {
        req = req.headers(extract_headers(op, opts)?);
    }
    req = apply_auth(req, opts);
    let resp = crate::gc::blocking_native(|| req.send())
        .map_err(|e| crate::error::RuntimeError::io_err(format!("{op} '{url}' failed: {e}")))?;
    let r = response_to_dict(resp);
    if r.is_ok() {
        vm.request_cooperative_yield();
    }
    r
}

fn http_get(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("get")?;
    let op = builtin_repr("get");
    let url = expect_text(&op, args, 0)?;
    let opts = args.get(1).cloned().unwrap_or(Value::None);
    let client = build_client(&opts)?;
    send_request(vm, &op, &url, &opts, client.get(url.as_str()))
}

fn http_post(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("post")?;
    let op = builtin_repr("post");
    let url = expect_text(&op, args, 0)?;
    let body = expect_text(&op, args, 1)?;
    let opts = args.get(2).cloned().unwrap_or(Value::None);
    let client = build_client(&opts)?;
    send_request(vm, &op, &url, &opts, client.post(url.as_str()).body(body))
}

fn http_put(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("put")?;
    let op = builtin_repr("put");
    let url = expect_text(&op, args, 0)?;
    let body = expect_text(&op, args, 1)?;
    let opts = args.get(2).cloned().unwrap_or(Value::None);
    let client = build_client(&opts)?;
    send_request(vm, &op, &url, &opts, client.put(url.as_str()).body(body))
}

fn http_delete(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("delete")?;
    let op = builtin_repr("delete");
    let url = expect_text(&op, args, 0)?;
    let opts = args.get(1).cloned().unwrap_or(Value::None);
    let client = build_client(&opts)?;
    send_request(vm, &op, &url, &opts, client.delete(url.as_str()))
}

fn http_patch(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("patch")?;
    let op = builtin_repr("patch");
    let url = expect_text(&op, args, 0)?;
    let body = expect_text(&op, args, 1)?;
    let opts = args.get(2).cloned().unwrap_or(Value::None);
    let client = build_client(&opts)?;
    send_request(vm, &op, &url, &opts, client.patch(url.as_str()).body(body))
}

fn http_head(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("head")?;
    let op = builtin_repr("head");
    let url = expect_text(&op, args, 0)?;
    let opts = args.get(1).cloned().unwrap_or(Value::None);
    let client = build_client(&opts)?;
    send_request(vm, &op, &url, &opts, client.head(url.as_str()))
}

fn http_request(vm: &mut Vm, args: &[Value]) -> Result<Value> {
    vm.caps.check_network("request")?;
    let op = builtin_repr("request");
    let method = expect_text(&op, args, 0)?;
    let url = expect_text(&op, args, 1)?;
    let opts = args.get(2).cloned().unwrap_or(Value::None);
    let client = build_client(&opts)?;
    let m = method.to_uppercase();
    let mut req_builder = match m.as_str() {
        "GET" => client.get(url.as_str()),
        "POST" => client.post(url.as_str()),
        "PUT" => client.put(url.as_str()),
        "DELETE" => client.delete(url.as_str()),
        "PATCH" => client.patch(url.as_str()),
        "HEAD" => client.head(url.as_str()),
        other => {
            return Err(crate::error::RuntimeError::type_err(format!(
                "{op}: unsupported method '{other}'"
            )));
        }
    };
    if let Value::Dict(d) = &opts {
        if let Some(Value::Text(s)) = d.borrow().get(&ValueKey::Text("body".into())) {
            req_builder = req_builder.body(s.clone());
        }
    }
    send_request(vm, &op, &url, &opts, req_builder)
}
