//! Minimal public-response fixtures; the HTTP tests use a local server and no
//! Steam credentials. Live tests are opt-in, like aurelia_integration.rs.
use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
    thread,
};

fn game() -> Value {
    serde_json::from_str(include_str!("../../tests/fixtures/hltb/portal.json")).unwrap()
}
fn page(game: Value) -> String {
    format!(
        r#"<script id="__NEXT_DATA__" type="application/json">{}</script>"#,
        json!({"props":{"pageProps":{"game":{"data":{"game":[game]}}}}})
    )
}
fn data() -> HltbData {
    verified_game(&page(game()), 400, 7230).unwrap().unwrap()
}

#[test]
fn verification_rejects_wrong_steam_and_page_ids_and_invalid_times() {
    assert_eq!(data().main, Some(11230));
    assert!(
        verified_game(&page(game()), 2012840, 7230)
            .unwrap()
            .is_some()
    );
    assert!(verified_game(&page(game()), 620, 7230).unwrap().is_none());
    assert!(verified_game(&page(game()), -1, 7230).is_err());
    assert!(verified_game(&page(game()), 0, 7230).is_err());
    assert!(verified_game(&page(game()), 400, 999).is_err());
    assert!(verified_game("<html>challenge</html>", 400, 7230).is_err());
    let mut g = game();
    g["profile_steam"] = json!("400");
    g["comp_main"] = json!(0);
    g["comp_plus"] = Value::Null;
    g.as_object_mut().unwrap().remove("comp_100");
    let d = verified_game(&page(g.clone()), 400, 7230).unwrap().unwrap();
    assert_eq!((d.main, d.extra, d.completionist), (None, None, None));
    g["comp_main"] = json!("broken");
    assert!(verified_game(&page(g), 400, 7230).is_err());
}

#[test]
fn versioned_cache_handles_expiry_corruption_and_failed_refreshes() {
    let dir = std::env::temp_dir().join(format!("aurelia-hltb-cache-{}", std::process::id()));
    let path = dir.join("400.json");
    let mut cached = Cached {
        version: CACHE_VERSION,
        steam_id: 400,
        fetched_at: DAY,
        data: Some(data()),
    };
    assert!(cached.fresh(8 * DAY - 1));
    assert!(!cached.fresh(8 * DAY));
    assert!(!cached.fresh(DAY - 1));
    write_cache(&path, &cached).unwrap();
    assert!(matches!(
        lookup_cached(Some(&path), 400, DAY, || panic!(
            "fresh data must skip network"
        )),
        HltbState::Ready { stale: false, .. }
    ));
    assert!(matches!(
        lookup_cached(Some(&path), 400, 9 * DAY, || Err("schema changed".into())),
        HltbState::Ready { stale: true, .. }
    ));
    assert_eq!(read_cache(&path, 400).unwrap().fetched_at, DAY);
    assert!(read_cache(&path, 620).is_none());
    cached.data = None;
    assert!(cached.fresh(2 * DAY - 1));
    assert!(!cached.fresh(2 * DAY));
    write_cache(&path, &cached).unwrap();
    assert!(matches!(
        lookup_cached(Some(&path), 400, DAY, || panic!(
            "fresh miss must skip network"
        )),
        HltbState::NotFound
    ));
    cached.version = 0;
    write_cache(&path, &cached).unwrap();
    assert!(read_cache(&path, 400).is_none());
    fs::write(&path, b"broken").unwrap();
    assert!(read_cache(&path, 400).is_none());
    fs::remove_file(&path).unwrap();
    assert!(
        matches!(lookup_cached(Some(&path), 400, DAY, || Err("HTTP 429".into())), HltbState::Unavailable(e) if e == "HTTP 429")
    );
    assert!(
        !path.exists(),
        "errors must never become negative cache entries"
    );
    assert!(matches!(
        lookup_cached(Some(&path), 400, now(), || Ok(None)),
        HltbState::NotFound
    ));
    assert!(read_cache(&path, 400).unwrap().fresh(now()));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn only_one_background_worker_can_be_reserved_and_it_is_released() {
    let first = reserve().expect("first worker");
    assert!(reserve().is_none());
    drop(first);
    assert!(reserve().is_some());
}

// A path prefix permits the init route's changing timestamp without weakening
// checks on the method, route, request headers, or posted search payload.
struct Step {
    method: &'static str,
    path: &'static str,
    status: u16,
    body: String,
}
impl Step {
    fn get(path: &'static str, body: impl Into<String>) -> Self {
        Self {
            method: "GET",
            path,
            status: 200,
            body: body.into(),
        }
    }
    fn post(body: impl Into<String>) -> Self {
        Self {
            method: "POST",
            path: "/api/search/site",
            status: 200,
            body: body.into(),
        }
    }
    fn status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }
}
// Serve only the scripted exchanges on localhost. Finite accept/read budgets
// make a missing request fail the test instead of hanging the test process.
struct Server {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
    worker: thread::JoinHandle<()>,
}
impl Server {
    fn start(steps: Vec<Step>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let worker = thread::spawn(move || {
            for step in steps {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(e) => panic!("missing {} {} request: {e}", step.method, step.path),
                    }
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut raw = Vec::new();
                let (header_end, length) = loop {
                    let mut buffer = [0u8; 4096];
                    let count = socket.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    raw.extend_from_slice(&buffer[..count]);
                    if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&raw[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        break (end + 4, length);
                    }
                };
                // TCP may split headers and JSON across separate reads. Do not
                // assert the search payload until Content-Length bytes arrived.
                while raw.len() < header_end + length {
                    let mut buffer = [0u8; 4096];
                    let count = socket.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    raw.extend_from_slice(&buffer[..count]);
                }
                let raw = String::from_utf8(raw).unwrap();
                assert!(
                    raw.starts_with(&format!("{} {}", step.method, step.path)),
                    "unexpected request: {raw}"
                );
                captured.lock().unwrap().push(raw);
                write!(
                    socket,
                    "HTTP/1.1 {} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    step.status,
                    step.body.len(),
                    step.body
                )
                .unwrap();
            }
        });
        Self {
            base,
            requests,
            worker,
        }
    }
    // Joining also surfaces assertion failures from the server thread; otherwise
    // an incorrectly ordered or missing request could leave a false-green test.
    fn finish(self) -> Vec<String> {
        self.worker.join().unwrap();
        Arc::try_unwrap(self.requests)
            .unwrap()
            .into_inner()
            .unwrap()
    }
}
fn discovery() -> Vec<Step> {
    vec![
        Step::get(
            "/",
            r#"<script src="/_next/static/chunks/pages/index-1.js"></script>"#,
        ),
        Step::get(
            "/_next/static/chunks/pages/index-1.js",
            r#"fetch("/api/search/site",{method:"POST",headers:{}})"#,
        ),
    ]
}
fn auth() -> Step {
    Step::get("/api/search/site/init?", r#"{"token":"fixture-token"}"#)
}
fn results(ids: &[u64]) -> String {
    json!({"data":ids.iter().map(|id| json!({"game_id":id,"game_name":"Portal"})).collect::<Vec<_>>()} ).to_string()
}

#[test]
fn http_lookup_checks_headers_payload_identity_and_reuses_endpoint() {
    let mut steps = discovery();
    for _ in 0..2 {
        steps.extend([
            auth(),
            Step::post(results(&[7230])),
            Step::get("/game/7230", page(game())),
        ]);
    }
    let server = Server::start(steps);
    let mut provider = Provider::new(&server.base).unwrap();
    for _ in 0..2 {
        assert_eq!(provider.fetch(400, "Portal").unwrap(), Some(data()));
    }
    let requests = server.finish();
    assert_eq!(requests.len(), 8);
    let request = &requests[3];
    assert!(
        request
            .to_ascii_lowercase()
            .contains("x-auth-token: fixture-token")
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("origin: http://127.0.0.1:")
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("referer: http://127.0.0.1:")
    );
    let payload: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(payload["searchTerms"], json!(["Portal"]));
    assert_eq!(payload["searchType"], "games");
}

#[test]
fn an_unreadable_candidate_does_not_hide_a_later_verified_match() {
    let mut steps = discovery();
    steps.extend([
        auth(),
        Step::post(results(&[1, 7230])),
        Step::get("/game/1", "broken page"),
        Step::get("/game/7230", page(game())),
    ]);
    let server = Server::start(steps);
    assert_eq!(
        Provider::new(&server.base)
            .unwrap()
            .fetch(400, "Portal")
            .unwrap(),
        Some(data())
    );
    server.finish();
}

#[test]
fn candidate_errors_are_not_reported_as_missing_games() {
    let mut steps = discovery();
    steps.extend([
        auth(),
        Step::post(results(&[1])),
        Step::get("/game/1", "broken page"),
    ]);
    let server = Server::start(steps);
    let error = Provider::new(&server.base)
        .unwrap()
        .fetch(400, "Portal")
        .unwrap_err();
    assert!(error.to_string().contains("page data missing"));
    server.finish();
}

#[test]
fn rate_limits_stop_requests_and_apply_a_session_cooldown() {
    let mut steps = discovery();
    steps.extend([auth(), Step::post("rate limited").status(429)]);
    let server = Server::start(steps);
    let mut provider = Provider::new(&server.base).unwrap();
    assert_eq!(
        provider.fetch(400, "Portal").unwrap_err().status,
        Some(StatusCode::TOO_MANY_REQUESTS)
    );
    assert!(
        provider
            .fetch(620, "Portal 2")
            .unwrap_err()
            .to_string()
            .contains("wait a minute")
    );
    assert_eq!(server.finish().len(), 4);
}

#[test]
fn a_changed_endpoint_is_rediscovered_once_after_a_404() {
    let mut steps = discovery();
    steps.push(auth().status(404));
    steps.extend(discovery());
    steps.extend([
        auth(),
        Step::post(results(&[7230])),
        Step::get("/game/7230", page(game())),
    ]);
    let server = Server::start(steps);
    assert!(
        Provider::new(&server.base)
            .unwrap()
            .fetch(400, "Portal")
            .unwrap()
            .is_some()
    );
    assert_eq!(server.finish().len(), 8);
}

#[test]
fn server_failures_are_not_retried_as_endpoint_changes() {
    let mut steps = discovery();
    steps.extend([auth(), Step::post("unavailable").status(503)]);
    let server = Server::start(steps);
    assert_eq!(
        Provider::new(&server.base)
            .unwrap()
            .fetch(400, "Portal")
            .unwrap_err()
            .status,
        Some(StatusCode::SERVICE_UNAVAILABLE)
    );
    assert_eq!(server.finish().len(), 4);
}

#[test]
fn schema_errors_empty_results_and_deadlines_are_distinct() {
    for (response, is_error) in [
        (r#"{"data":[]}"#, false),
        (r#"{"unexpected":[]}"#, true),
        ("not JSON", true),
    ] {
        let mut steps = discovery();
        steps.extend([auth(), Step::post(response)]);
        let server = Server::start(steps);
        let result = Provider::new(&server.base).unwrap().fetch(400, "Portal");
        if is_error {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_none());
        }
        server.finish();
    }
    let provider = Provider::new(BASE).unwrap();
    assert!(
        provider
            .request("/", Instant::now() - Duration::from_secs(1))
            .is_err()
    );
}

#[test]
#[ignore = "requires live HowLongToBeat access"]
fn live_steam_matches() {
    let mut provider = Provider::new(BASE).unwrap();
    for (id, name) in [
        (400, "Portal"),
        (1306630, "Lost Ruins"),
        (599140, "Graveyard Keeper"),
    ] {
        let data = provider
            .fetch(id, name)
            .unwrap()
            .expect("verified HLTB match");
        assert!(data.main.is_some(), "{name}");
        println!(
            "{name}: {:?} / {:?} / {:?} seconds ({})",
            data.main,
            data.extra,
            data.completionist,
            data.url()
        );
    }
}

#[test]
fn missing_tokens_do_not_trigger_a_second_discovery_or_a_search() {
    let mut steps = discovery();
    steps.push(Step::get("/api/search/site/init?", "{}"));
    let server = Server::start(steps);
    assert!(
        Provider::new(&server.base)
            .unwrap()
            .fetch(400, "Portal")
            .unwrap_err()
            .to_string()
            .contains("token missing")
    );
    assert_eq!(server.finish().len(), 3);
}

#[test]
fn a_slow_response_obeys_the_remaining_lookup_budget() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut buffer = [0u8; 4096];
        assert!(socket.read(&mut buffer).unwrap() > 0);
        thread::sleep(Duration::from_millis(150));
        let _ = socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
    });
    let provider = Provider::new(&base).unwrap();
    let response = provider
        .request("/", Instant::now() + Duration::from_millis(30))
        .unwrap()
        .send();
    let error = Provider::body(response, "slow fixture").unwrap_err();
    assert!(error.to_string().contains("timed out"), "{error}");
    worker.join().unwrap();
}
