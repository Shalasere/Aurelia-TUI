//! Optional HowLongToBeat metadata. The UI schedules this after selection settles,
//! just like ProtonDB; only matches with a verified Steam app ID are displayed.
use std::{
    fs,
    io::Read,
    path::Path,
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use regex::Regex;
use reqwest::{
    StatusCode,
    blocking::{Client, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::util::{error::STError, log::log, paths::cache_directory};

const BASE: &str = "https://howlongtobeat.com";
const DAY: u64 = 86_400;
const CACHE_VERSION: u8 = 1;
const MAX_CANDIDATES: usize = 3;
const MAX_BODY: u64 = 8 * 1024 * 1024;
// The public website rejects requests with reqwest's default user agent.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
static BUSY: AtomicBool = AtomicBool::new(false);
static PROVIDER: Mutex<Option<Provider>> = Mutex::new(None);
static SCRIPTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<script[^>]+src=["']([^"']+)["']"#).unwrap());
static ENDPOINT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"fetch\s*\(\s*["'](/api/[a-zA-Z0-9_/]+)[^"']*["']\s*,\s*\{[^}]*method:\s*["']POST["']"#,
    )
    .unwrap()
});
static PAGE_DATA: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)<script\b[^>]*\bid=["']__NEXT_DATA__["'][^>]*>(.*?)</script>"#).unwrap()
});

pub(crate) struct Permit;
impl Drop for Permit {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

/// A single worker bounds request volume without queuing games scrolled past.
pub(crate) fn reserve() -> Option<Permit> {
    BUSY.compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .ok()
        .map(|_| Permit)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HltbData {
    pub game_id: u64,
    pub game_name: String,
    /// Completion times in seconds. None means no submitted time for that style.
    pub main: Option<u64>,
    pub extra: Option<u64>,
    pub completionist: Option<u64>,
}

impl HltbData {
    pub fn url(&self) -> String {
        format!("{BASE}/game/{}", self.game_id)
    }
}

#[derive(Debug, Clone, Default)]
pub enum HltbState {
    #[default]
    Pending,
    Loading,
    Ready {
        data: HltbData,
        stale: bool,
    },
    NotFound,
    Unavailable(String),
}

#[derive(Serialize, Deserialize)]
struct Cached {
    version: u8,
    steam_id: i32,
    fetched_at: u64,
    data: Option<HltbData>,
}

impl Cached {
    fn fresh(&self, now: u64) -> bool {
        let ttl = if self.data.is_some() { 7 * DAY } else { DAY };
        now.checked_sub(self.fetched_at)
            .is_some_and(|age| age < ttl)
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn read_cache(path: &Path, steam_id: i32) -> Option<Cached> {
    let cached: Cached = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (cached.version == CACHE_VERSION
        && cached.steam_id == steam_id
        && cached
            .data
            .as_ref()
            .is_none_or(|d| d.game_id > 0 && !d.game_name.trim().is_empty()))
    .then_some(cached)
}

fn write_cache(path: &Path, cached: &Cached) -> Result<(), STError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    // Same-directory rename avoids partially written entries after interruption.
    let result = (|| {
        fs::write(&temp, serde_json::to_vec(cached)?)?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    let _ = fs::remove_file(temp);
    result
}

pub(crate) fn lookup(steam_id: i32, name: &str) -> HltbState {
    let path = cache_directory()
        .ok()
        .map(|p| p.join("hltb").join(format!("{steam_id}.json")));
    lookup_cached(path.as_deref(), steam_id, now(), || {
        let mut provider = PROVIDER.lock().map_err(|e| e.to_string())?;
        if provider.is_none() {
            *provider = Some(Provider::new(BASE)?);
        }
        provider
            .as_mut()
            .unwrap()
            .fetch(steam_id, name)
            .map_err(|e| e.to_string())
    })
}

fn lookup_cached(
    path: Option<&Path>,
    steam_id: i32,
    timestamp: u64,
    fetch: impl FnOnce() -> Result<Option<HltbData>, String>,
) -> HltbState {
    let cached = path.and_then(|p| read_cache(p, steam_id));
    if let Some(c) = &cached
        && c.fresh(timestamp)
    {
        return match &c.data {
            Some(data) => HltbState::Ready {
                data: data.clone(),
                stale: false,
            },
            None => HltbState::NotFound,
        };
    }
    match fetch() {
        Ok(data) => {
            if let Some(path) = path {
                let cached = Cached {
                    version: CACHE_VERSION,
                    steam_id,
                    fetched_at: now(),
                    data: data.clone(),
                };
                if let Err(error) = write_cache(path, &cached) {
                    log!("HLTB cache write failed", steam_id, error);
                }
            }
            match data {
                Some(data) => HltbState::Ready { data, stale: false },
                None => HltbState::NotFound,
            }
        }
        Err(error) => {
            log!("HLTB lookup failed", steam_id, error);
            match cached.and_then(|c| c.data) {
                Some(data) => HltbState::Ready { data, stale: true },
                None => HltbState::Unavailable(error),
            }
        }
    }
}

#[derive(Debug)]
struct FetchError {
    context: String,
    status: Option<StatusCode>,
}
impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.context)
    }
}
impl std::error::Error for FetchError {}

impl From<String> for FetchError {
    fn from(context: String) -> Self {
        Self {
            context,
            status: None,
        }
    }
}
impl From<&str> for FetchError {
    fn from(context: &str) -> Self {
        context.to_string().into()
    }
}

type FetchResult<T> = Result<T, FetchError>;

struct Provider {
    client: Client,
    base: String,
    endpoint: Option<String>,
    retry_after: Option<Instant>,
}

impl Provider {
    fn new(base: &str) -> Result<Self, String> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::REFERER,
            format!("{base}/")
                .parse()
                .map_err(|e| format!("HLTB referer: {e}"))?,
        );
        headers.insert(
            reqwest::header::ORIGIN,
            base.parse().map_err(|e| format!("HLTB origin: {e}"))?,
        );
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            base: base.into(),
            endpoint: None,
            retry_after: None,
        })
    }

    fn request(
        &self,
        path: &str,
        deadline: Instant,
    ) -> FetchResult<reqwest::blocking::RequestBuilder> {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or("HLTB lookup timed out")?;
        Ok(self
            .client
            .get(format!("{}{path}", self.base))
            .timeout(remaining.min(Duration::from_secs(8))))
    }

    fn body(response: reqwest::Result<Response>, context: &str) -> FetchResult<String> {
        let response = response.map_err(|e| FetchError {
            context: format!("{context}: {e}"),
            status: e.status(),
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(FetchError {
                context: format!("{context}: HTTP {status}"),
                status: Some(status),
            });
        }
        let mut text = String::new();
        response
            .take(MAX_BODY + 1)
            .read_to_string(&mut text)
            .map_err(|e| format!("{context}: {e}"))?;
        if text.len() as u64 > MAX_BODY {
            return Err(format!("{context}: response too large").into());
        }
        Ok(text)
    }

    fn text(&self, path: &str, deadline: Instant) -> FetchResult<String> {
        Self::body(self.request(path, deadline)?.send(), "HLTB page request")
    }

    fn discover(&self, deadline: Instant) -> FetchResult<String> {
        let html = self.text("/", deadline)?;
        let mut paths: Vec<_> = SCRIPTS
            .captures_iter(&html)
            .map(|c| c[1].to_owned())
            .filter(|p| p.starts_with("/_next/static/chunks/"))
            .collect();
        paths.sort_by_key(|p| !p.contains("pages/index-"));
        for path in paths.into_iter().take(12) {
            // A missing chunk can follow a site deployment; other HTTP failures
            // should surface immediately, especially rate limiting.
            let script = match self.text(&path, deadline) {
                Ok(script) => script,
                Err(e) if e.status == Some(StatusCode::NOT_FOUND) => continue,
                Err(e) => return Err(e),
            };
            if let Some(c) = ENDPOINT.captures(&script) {
                return Ok(c[1].trim_end_matches('/').into());
            }
        }
        Err("HLTB search endpoint not found in site scripts".into())
    }

    fn search(&self, endpoint: &str, name: &str, deadline: Instant) -> FetchResult<Value> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let auth = self.text(&format!("{endpoint}/init?t={nonce}"), deadline)?;
        let auth: Value =
            serde_json::from_str(&auth).map_err(|e| format!("HLTB search token response: {e}"))?;
        let token = auth
            .get("token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .ok_or("HLTB search token missing")?;
        let payload = json!({
            "searchType": "games", "searchTerms": name.split_whitespace().collect::<Vec<_>>(),
            "searchPage": 1, "size": 20, "useCache": true,
            "searchOptions": {
                "games": {"userId": 0, "platform": "", "sortCategory": "popular",
                    "rangeCategory": "main", "rangeTime": {"min": 0, "max": 0},
                    "gameplay": {"perspective": "", "flow": "", "genre": "", "difficulty": ""},
                    "rangeYear": {"min": "", "max": ""}, "modifier": ""},
                "users": {"sortCategory": "postcount"}, "lists": {"sortCategory": "follows"},
                "filter": "", "sort": 0, "randomizer": 0
            }
        });
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or("HLTB lookup timed out")?;
        let response = self
            .client
            .post(format!("{}{endpoint}", self.base))
            .header("x-auth-token", token)
            .timeout(remaining.min(Duration::from_secs(8)))
            .json(&payload)
            .send();
        let body = Self::body(response, "HLTB search request")?;
        serde_json::from_str(&body).map_err(|e| format!("HLTB search response: {e}").into())
    }

    fn fetch(&mut self, steam_id: i32, name: &str) -> FetchResult<Option<HltbData>> {
        if steam_id <= 0 || name.trim().is_empty() {
            return Err("Invalid Steam game identity".into());
        }
        if self.retry_after.is_some_and(|t| t > Instant::now()) {
            return Err("HLTB rate limited; wait a minute before retrying".into());
        }
        let result = self.fetch_inner(steam_id, name, Instant::now() + Duration::from_secs(30));
        if result
            .as_ref()
            .err()
            .is_some_and(|e| e.status == Some(StatusCode::TOO_MANY_REQUESTS))
        {
            self.retry_after = Some(Instant::now() + Duration::from_secs(60));
        }
        result
    }

    fn fetch_inner(
        &mut self,
        steam_id: i32,
        name: &str,
        deadline: Instant,
    ) -> FetchResult<Option<HltbData>> {
        if self.endpoint.is_none() {
            self.endpoint = Some(self.discover(deadline)?);
        }
        let response = match self.search(self.endpoint.as_deref().unwrap(), name, deadline) {
            Ok(response) => response,
            Err(e)
                if matches!(
                    e.status,
                    Some(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND)
                ) =>
            {
                self.endpoint = None;
                self.endpoint = Some(self.discover(deadline)?);
                self.search(self.endpoint.as_deref().unwrap(), name, deadline)?
            }
            Err(e) => return Err(e),
        };
        let games = response
            .get("data")
            .and_then(Value::as_array)
            .ok_or("HLTB search results missing")?;
        let mut candidates: Vec<_> = games.iter().collect();
        let title = normalized(name);
        candidates.sort_by_key(|g| {
            !(normalized(g["game_name"].as_str().unwrap_or("")) == title
                || g["game_alias"]
                    .as_str()
                    .unwrap_or("")
                    .split(',')
                    .any(|a| normalized(a) == title))
        });
        let mut last_error = None;
        for game in candidates.into_iter().take(MAX_CANDIDATES) {
            let candidate = (|| {
                let id = number(&game["game_id"])
                    .filter(|id| *id > 0)
                    .ok_or("HLTB result ID missing")?;
                let html = self.text(&format!("/game/{id}"), deadline)?;
                verified_game(&html, steam_id, id).map_err(FetchError::from)
            })();
            match candidate {
                Ok(Some(data)) => return Ok(Some(data)),
                Ok(None) => {}
                Err(e) if e.status == Some(StatusCode::TOO_MANY_REQUESTS) => return Err(e),
                Err(e) => last_error = Some(e),
            }
        }
        // Failed verification requests are errors, not cacheable negative matches.
        match last_error {
            Some(e) => Err(e),
            None => Ok(None),
        }
    }
}

fn normalized(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn number(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn seconds(game: &Value, key: &str) -> Result<Option<u64>, String> {
    match game.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => number(value)
            .map(|n| (n > 0).then_some(n))
            .ok_or_else(|| format!("HLTB {key}: invalid completion time")),
    }
}

fn verified_game(html: &str, steam_id: i32, expected_id: u64) -> Result<Option<HltbData>, String> {
    let steam_id = u64::try_from(steam_id)
        .ok()
        .filter(|id| *id > 0)
        .ok_or("Invalid Steam app ID")?;
    let data = PAGE_DATA.captures(html).ok_or("HLTB page data missing")?;
    let data: Value = serde_json::from_str(&data[1]).map_err(|e| format!("HLTB page data: {e}"))?;
    let games = data
        .pointer("/props/pageProps/game/data/game")
        .and_then(Value::as_array)
        .ok_or("HLTB game data missing")?;
    for game in games {
        if !["profile_steam", "profile_steam_alt"]
            .iter()
            .any(|key| number(&game[key]) == Some(steam_id))
        {
            continue;
        }
        let game_id = number(&game["game_id"])
            .filter(|id| *id > 0)
            .ok_or("HLTB game ID missing")?;
        if game_id != expected_id {
            return Err("HLTB returned a different game page".into());
        }
        let name = game["game_name"]
            .as_str()
            .filter(|n| !n.trim().is_empty())
            .ok_or("HLTB title missing")?;
        return Ok(Some(HltbData {
            game_id,
            game_name: name.into(),
            main: seconds(game, "comp_main")?,
            extra: seconds(game, "comp_plus")?,
            completionist: seconds(game, "comp_100")?,
        }));
    }
    Ok(None)
}

#[cfg(test)]
#[path = "hltb_tests.rs"]
mod tests;
