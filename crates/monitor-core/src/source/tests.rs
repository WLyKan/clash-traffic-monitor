use super::*;
use serde_json::json;
use std::{
    collections::VecDeque,
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
};

struct Script {
    rows: VecDeque<(&'static str, Result<Value, String>, Option<u32>)>,
    pid: Option<u32>,
    calls: Arc<Mutex<Vec<String>>>,
}
impl Transport for Script {
    fn get(&mut self, path: &str) -> Result<Value, String> {
        self.calls.lock().unwrap().push(path.into());
        let (expected, payload, pid) = self.rows.pop_front().expect("unexpected request");
        assert_eq!(path, expected);
        self.pid = pid;
        payload
    }
    fn server_pid(&self) -> Option<u32> {
        self.pid
    }
}
fn script(
    rows: Vec<(&'static str, Result<Value, String>, Option<u32>)>,
) -> (Box<dyn Transport>, Arc<Mutex<Vec<String>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    (
        Box::new(Script {
            rows: rows.into(),
            pid: None,
            calls: calls.clone(),
        }),
        calls,
    )
}
fn connections(rows: Value) -> Value {
    json!({"uploadTotal":100,"downloadTotal":200,"connections":rows})
}
fn row(id: &str, leaf: &str) -> Value {
    json!({"id":id,"upload":10,"download":20,"chains":[leaf,"Selector"],
    "start":"2026-11-12T01:02:03.123456789+08:00","metadata":{"processPath":"C:\\Apps\\Code.exe","host":"EXAMPLE.COM."}})
}

#[test]
fn parser_cache_dedup_and_first_leaf_contract() {
    let proxies = json!({"proxies":{"Node":{"type":"Trojan"},"DIRECT":{"type":"Direct"},"Selector":{"type":"Selector"}}});
    let mut invalid = row("bad", "Node");
    invalid["upload"] = json!(true);
    let (endpoint, calls) = script(vec![
        ("/proxies", Ok(proxies), None),
        (
            "/connections",
            Ok(connections(json!([
                row("a", "Node"),
                row("b", "DIRECT"),
                row("c", "Selector"),
                invalid,
                row("a", "DIRECT"),
                null,
                42
            ]))),
            None,
        ),
        ("/connections", Ok(connections(json!([]))), None),
    ]);
    let mut source = MihomoSource::with_transports(vec![endpoint]);
    let snapshot = source.sample().unwrap();
    assert_eq!(snapshot.connections.len(), 3);
    assert_eq!(
        snapshot
            .connections
            .iter()
            .map(|c| c.route)
            .collect::<Vec<_>>(),
        vec![Route::Proxy, Route::Direct, Route::Unknown]
    );
    assert_eq!(snapshot.connections[0].app.as_deref(), Some("Code.exe"));
    assert_eq!(snapshot.connections[0].target, "example.com");
    assert_eq!(
        snapshot.connections[0].started_at,
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-11-12T01:02:03.123456+08:00")
                .unwrap()
                .timestamp_micros() as f64
                / 1e6
        )
    );
    assert!(snapshot.completed_at.unwrap() >= snapshot.sampled_at);
    source.sample().unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["/proxies", "/connections", "/connections"]
    );
}

#[test]
fn provider_qualified_routes_disambiguate_same_named_nodes() {
    let mut a = row("a", "Same");
    a["providerChains"] = json!(["A"]);
    let mut b = row("b", "Same");
    b["providerChains"] = json!(["B"]);
    let (endpoint, _) = script(vec![
        ("/proxies", Ok(json!({"proxies":{}})), None),
        (
            "/connections",
            Ok(connections(json!([
                a,
                b,
                row("c", "Same"),
                row("d", "Unique")
            ]))),
            None,
        ),
        (
            "/providers/proxies",
            Ok(
                json!({"providers":{"A":{"proxies":[{"name":"Same","type":"Trojan"},{"name":"Unique","type":"VMess"}]},
            "B":{"proxies":[{"name":"Same","type":"Direct"}]}}}),
            ),
            None,
        ),
    ]);
    let mut source = MihomoSource::with_transports(vec![endpoint]);
    assert_eq!(
        source
            .sample()
            .unwrap()
            .connections
            .iter()
            .map(|c| c.route)
            .collect::<Vec<_>>(),
        vec![Route::Proxy, Route::Direct, Route::Unknown, Route::Proxy]
    );
}

#[test]
fn provider_failure_is_unknown_and_never_guesses_selector() {
    let (endpoint, _) = script(vec![
        (
            "/proxies",
            Ok(json!({"proxies":{"Selector":{"type":"Selector"}}})),
            None,
        ),
        (
            "/connections",
            Ok(connections(json!([row("a", "Absent")]))),
            None,
        ),
        ("/providers/proxies", Err("offline".into()), None),
    ]);
    assert_eq!(
        MihomoSource::with_transports(vec![endpoint])
            .sample()
            .unwrap()
            .connections[0]
            .route,
        Route::Unknown
    );
}

#[test]
fn server_pid_change_increments_generation_and_invalidates_routes_for_one_sample() {
    let proxies = json!({"proxies":{"Node":{"type":"Trojan"}}});
    let payload = connections(json!([row("a", "Node")]));
    let (endpoint, _) = script(vec![
        ("/proxies", Ok(proxies.clone()), Some(11)),
        ("/connections", Ok(payload.clone()), Some(11)),
        ("/connections", Ok(payload.clone()), Some(22)),
        ("/providers/proxies", Ok(json!({})), Some(22)),
        ("/proxies", Ok(proxies), Some(22)),
        ("/connections", Ok(payload), Some(22)),
    ]);
    let mut source = MihomoSource::with_transports(vec![endpoint]);
    let values = (0..3)
        .map(|_| {
            let sample = source.sample().unwrap();
            (sample.generation, sample.connections[0].route)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        values,
        vec![(0, Route::Proxy), (1, Route::Unknown), (1, Route::Proxy)]
    );
}

#[test]
fn endpoint_failover_changes_generation_and_retains_working_endpoint() {
    let proxies = json!({"proxies":{}});
    let payload = connections(json!([]));
    let (first, _) = script(vec![
        ("/proxies", Ok(proxies.clone()), None),
        ("/connections", Ok(payload.clone()), None),
        ("/connections", Err("offline".into()), None),
    ]);
    let (second, calls) = script(vec![
        ("/proxies", Ok(proxies), None),
        ("/connections", Ok(payload.clone()), None),
        ("/connections", Ok(payload), None),
    ]);
    let mut source = MihomoSource::with_transports(vec![first, second]);
    assert_eq!(source.sample().unwrap().generation, 0);
    assert_eq!(source.sample().unwrap().generation, 1);
    assert_eq!(source.sample().unwrap().generation, 1);
    assert_eq!(calls.lock().unwrap().len(), 3);
}

#[test]
fn malformed_snapshot_never_advances_generation_or_accepts_boolean_counter() {
    let (endpoint, _) = script(vec![
        ("/proxies", Ok(json!({"proxies":{}})), None),
        (
            "/connections",
            Ok(json!({"uploadTotal":true,"downloadTotal":0,"connections":[]})),
            None,
        ),
    ]);
    let mut source = MihomoSource::with_transports(vec![endpoint]);
    assert!(source.sample().is_err());
    assert_eq!(source.generation, 0);
    assert!(source.active.is_none());
}

#[test]
fn exact_tcp_four_tuple_pid_start_guard_and_cache() {
    let mut a = row("a", "Node");
    a["metadata"] = json!({"network":"TCP","sourceIP":"::ffff:127.0.0.1","sourcePort":"53000","inboundIP":"127.0.0.1","inboundPort":7890});
    let key = connection_socket(&a["metadata"]).unwrap();
    let mut source = MihomoSource::with_transports(vec![]);
    let result = source.connections_with(
        &[a.clone()],
        |needed| {
            assert_eq!(needed, &HashSet::from([key]));
            HashMap::from([(key, Some(15))])
        },
        |pid| {
            assert_eq!(pid, 15);
            Some(("fallback.exe".into(), 0.0))
        },
    );
    assert_eq!(result[0].app.as_deref(), Some("fallback.exe"));
    let cached = source.connections_with(
        &[a.clone()],
        |needed| {
            assert!(needed.is_empty());
            HashMap::new()
        },
        |_| panic!("cache miss"),
    );
    assert_eq!(cached[0].app, result[0].app);
    source.apps.clear();
    assert_eq!(
        source.connections_with(
            &[a.clone()],
            |_| HashMap::from([(key, None)]),
            |_| panic!("ambiguous pid")
        )[0]
        .app,
        None
    );
    assert_eq!(
        source.connections_with(
            &[a],
            |_| HashMap::from([(key, Some(15))]),
            |_| Some(("reused.exe".into(), f64::MAX))
        )[0]
        .app,
        None
    );
    assert!(connection_socket(&json!({"network":"udp"})).is_none());
    assert!(normalize_address("127.0.0.1", 0).is_none());
    assert_eq!(
        normalize_address("::ffff:127.0.0.1", 1),
        normalize_address("127.0.0.1", 1)
    );
}

#[test]
fn controller_must_be_authenticated_loopback_with_explicit_port() {
    for endpoint in [
        "http://example.com:9090",
        "https://127.0.0.1:9090",
        "http://127.0.0.1",
        "http://127.0.0.1:0",
        "http://user:pass@127.0.0.1:9090",
        "http://127.0.0.1:9090/path",
        "http://127.0.0.1:9090?x=1",
        "http://127.0.0.1:9090#x",
    ] {
        assert!(HttpEndpoint::new(endpoint, "secret").is_err(), "{endpoint}");
    }
    for secret in ["", " ", "top-secret\r\nInjected: yes", "秘密"] {
        let error = HttpEndpoint::new("127.0.0.1:9090", secret).err().unwrap();
        if !secret.is_empty() && !secret.trim().is_empty() {
            assert!(!error.contains(secret));
        }
    }
    for endpoint in ["http://localhost:9090", "127.0.0.1:80", "http://[::1]:9090"] {
        assert!(HttpEndpoint::new(endpoint, "secret").is_ok());
    }
}

#[test]
fn real_loopback_http_auth_chunked_unicode_and_keepalive_length() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (close_tx, close_rx) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut data = Vec::new();
        let mut scratch = [0; 1024];
        while !data.windows(4).any(|part| part == b"\r\n\r\n") {
            let count = stream.read(&mut scratch).unwrap();
            assert_ne!(count, 0);
            data.extend_from_slice(&scratch[..count]);
        }
        let request = String::from_utf8(data).unwrap();
        assert!(request.contains("Authorization: Bearer fixture-secret\r\n"));
        let body = "{\"目标\":\"例子\"}".as_bytes();
        stream.write_all(format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n{:x}\r\n",body.len()).as_bytes()).unwrap();
        stream.write_all(body).unwrap();
        stream.write_all(b"\r\n0\r\n\r\n").unwrap();
        // The caller must finish on the terminal chunk, without waiting for EOF.
        close_rx.recv_timeout(Duration::from_secs(4)).unwrap();
    });
    let mut endpoint = HttpEndpoint::new(&address.to_string(), "fixture-secret").unwrap();
    let now = Instant::now();
    let payload = endpoint.get("/connections").unwrap();
    assert!(now.elapsed() < Duration::from_secs(2));
    assert_eq!(payload, json!({"目标":"例子"}));
    close_tx.send(()).unwrap();
    server.join().unwrap();
}

#[test]
fn framing_rejects_bad_json_truncation_and_oversize_chunks() {
    assert_eq!(
        decode_chunks(b"2\r\n{}\r\n0\r\n\r\n").unwrap(),
        Some(b"{}".to_vec())
    );
    assert_eq!(decode_chunks(b"2\r\n{").unwrap(), None);
    assert!(decode_chunks(b"2\r\n{}xx").is_err());
    assert!(decode_chunks(b"2000001\r\n").is_err());
    assert!(parse_json(b"[]").is_err());
    assert!(parse_json(b"{oops}").is_err());
    assert_eq!(
        app_name(" C:\\Apps\\Code.exe ").as_deref(),
        Some("Code.exe")
    );
    assert!(app_name("bad\n.exe").is_none());
}
