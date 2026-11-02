//! Google Custom Search Engine, ported from searx/engines/google_cse.py.
// engine-id: google_cse

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::engine::util::*;
use crate::engine::{
    Engine, EngineDefaults, EngineError, EngineItem, EngineMeta, EngineRequest, EngineResponse,
};
use crate::query::{Query, TimeRange};
use crate::results::Template;
use crate::settings::EngineConfig;

const DEFAULTS: EngineDefaults = EngineDefaults {
    categories: &["general", "web"],
    shortcut: "goc",
    paging: true,
    time_range_support: true,
    safesearch: true,
    language_support: true,
    weight: 1.0,
    timeout: 4.0,
};

const ENDPOINT: &str = "https://cse.google.com/cse/element/v1";
/// The JavaScript the element endpoint hands out the token in.
const CSE_JS: &str = "https://www.google.com/cse/cse.js";
const DEFAULT_CX: &str = "partner-pub-8993703457585266:4862972284";
const DEFAULT_PAGE_SIZE: i64 = 20;
/// Upstream caches the token for 3600 s (`CACHE.set(CX, token, expire=3600)`).
const TOKEN_TTL: Duration = Duration::from_secs(3600);

/// The token state extracted from `cse.js`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CseToken {
    cse_tok: String,
    cselibv: String,
    exp: String,
}

struct CachedToken {
    token: CseToken,
    expires_at: Instant,
}

/// Google CSE, the JSONP element endpoint of a custom search engine.
///
/// The token is configured (`cse_tok`) or fetched from `cse.js` by the
/// preflight request and cached in-process, like upstream. `google_categ`
/// selects the web (`""`) or the image (`"image"`) item mapping.
pub struct GoogleCse {
    meta: EngineMeta,
    /// Upstream `google_categ`: `""` for web, `"image"` for images.
    google_categ: String,
    /// Token cache, unused while a `cse_tok` is configured.
    token: Mutex<Option<CachedToken>>,
}

impl GoogleCse {
    pub fn from_config(config: &EngineConfig) -> Result<Arc<Self>, String> {
        let google_categ = config.opt_str("google_categ").unwrap_or("").trim();
        if !matches!(google_categ, "" | "image") {
            return Err(format!("invalid google cse category: {google_categ}"));
        }
        Ok(Arc::new(Self {
            meta: EngineMeta::from_config(config, DEFAULTS),
            google_categ: google_categ.to_string(),
            token: Mutex::new(None),
        }))
    }

    /// The partner CX, configured or the upstream default.
    fn cx(&self) -> &str {
        self.meta.opt_str("cx").unwrap_or(DEFAULT_CX)
    }

    /// An explicitly configured token wins over the preflight, upstream has no
    /// such override but the port has always accepted one.
    fn configured_token(&self) -> Option<&str> {
        self.meta
            .opt_str("cse_tok")
            .map(str::trim)
            .filter(|token| !token.is_empty())
    }

    fn cached_token(&self) -> Option<CseToken> {
        let cached = self.token.lock().ok()?;
        let cached = cached.as_ref()?;
        (cached.expires_at > Instant::now()).then(|| cached.token.clone())
    }

    fn store_token(&self, token: CseToken) {
        if let Ok(mut cached) = self.token.lock() {
            *cached = Some(CachedToken {
                token,
                expires_at: Instant::now() + TOKEN_TTL,
            });
        }
    }

    fn page_size(&self) -> i64 {
        self.meta
            .opt_i64("page_size")
            .filter(|size| *size > 0)
            .unwrap_or(DEFAULT_PAGE_SIZE)
    }
}

fn is_http(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// Upstream web_item: a web result of the element endpoint.
fn web_item(item: &serde_json::Value) -> Option<EngineItem> {
    let url = json_str(item, "unescapedUrl");
    let title = json_str(item, "titleNoFormatting");
    if !is_http(&url) || title.is_empty() {
        return None;
    }
    let content = json_str(item, "contentNoFormatting");
    let mut builder = ResultBuilder::new(url, title).content(content);
    let thumbnail = json_str(item, "richSnippet.cseThumbnail.src");
    if is_http(&thumbnail) {
        builder = builder.thumbnail(thumbnail);
    }
    Some(builder.build())
}

/// Upstream img_item: an image result of the element endpoint.
///
/// The target page (`originalContextUrl`) is the result URL and the image
/// itself (`unescapedUrl`) becomes `img_src`; `tbUrl` is the thumbnail. The
/// resolution and the file format have no dedicated field in this result
/// model, so they are carried as metadata like the other image engines do.
fn image_item(item: &serde_json::Value) -> Option<EngineItem> {
    let url = json_str(item, "originalContextUrl");
    let img_src = json_str(item, "unescapedUrl");
    if !is_http(&url) || !is_http(&img_src) {
        return None;
    }
    let title = json_str(item, "titleNoFormatting");
    let content = json_str(item, "contentNoFormatting");
    let mut builder = ResultBuilder::new(url, title)
        .content(content)
        .image(img_src)
        .template(Template::Images);
    let thumbnail = json_str(item, "tbUrl");
    if is_http(&thumbnail) {
        builder = builder.thumbnail(thumbnail);
    }
    let width = json_str(item, "width");
    let height = json_str(item, "height");
    if !width.is_empty() && !height.is_empty() {
        builder = builder.metadata("resolution", format!("{width} x {height}"));
    }
    let file_format = json_str(item, "fileFormat");
    if let Some(format) = file_format
        .rsplit('/')
        .next()
        .filter(|part| !part.is_empty())
    {
        builder = builder.metadata("img_format", format);
    }
    Some(builder.build())
}

impl Engine for GoogleCse {
    fn meta(&self) -> &EngineMeta {
        &self.meta
    }

    fn preflight_request(&self, _query: &Query, _pageno: u32) -> Option<EngineRequest> {
        if self.configured_token().is_some() || self.cached_token().is_some() {
            return None;
        }
        Some(EngineRequest::get(format!("{CSE_JS}?cx={}", self.cx())))
    }

    fn preflight(&self, response: &EngineResponse, _query: &Query) -> Result<(), EngineError> {
        if response.status == 429 {
            return Err(EngineError::TooManyRequests);
        }
        if !(200..300).contains(&response.status) {
            return Err(EngineError::Message(format!(
                "google cse: failed to obtain cse token (HTTP {})",
                response.status
            )));
        }
        // Upstream takes the last `({ ... });` object of the JavaScript.
        let text = response.text();
        let start = text
            .rfind("({")
            .ok_or_else(|| EngineError::Parse("google cse: cse.js has no token payload".into()))?;
        let end = text
            .rfind("});")
            .ok_or_else(|| EngineError::Parse("google cse: cse.js has no token payload".into()))?;
        if end < start {
            return Err(EngineError::Parse(
                "google cse: cse.js token payload is malformed".into(),
            ));
        }
        let payload = text.get(start + 1..=end).ok_or_else(|| {
            EngineError::Parse("google cse: cse.js token payload is malformed".into())
        })?;
        let json: serde_json::Value = serde_json::from_str(payload).map_err(|error| {
            EngineError::Parse(format!("google cse: invalid cse.js payload: {error}"))
        })?;
        let cse_tok = json_str(&json, "cse_token");
        if cse_tok.trim().is_empty() {
            return Err(EngineError::Message(
                "google cse: failed to obtain cse token".into(),
            ));
        }
        let cselibv = json_str(&json, "cselibVersion");
        let exp = json_path(&json, "exp")
            .and_then(|value| value.as_array())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        self.store_token(CseToken {
            cse_tok,
            cselibv,
            exp,
        });
        Ok(())
    }

    fn build_request(&self, query: &Query, pageno: u32) -> Result<EngineRequest, EngineError> {
        let (cse_tok, cselibv, exp) = match self.configured_token() {
            Some(configured) => (configured.to_string(), String::new(), String::new()),
            None => {
                let cached = self.cached_token().ok_or_else(|| {
                    EngineError::Message(
                        "google cse needs a cse_tok (the token preflight did not run)".into(),
                    )
                })?;
                (cached.cse_tok, cached.cselibv, cached.exp)
            }
        };
        let page_size = self.page_size();
        let locale_language = locale_language(&query.locale);
        let language = if locale_language == "all" {
            "en".to_string()
        } else {
            locale_language.clone()
        };
        let region = locale_region(&query.locale);
        let safe = match query.safesearch {
            0 => "off",
            1 => "medium",
            _ => "high",
        };
        let mut pairs: Vec<(&str, String)> = vec![
            ("rsz", "filtered_cse".to_string()),
            ("num", page_size.to_string()),
            ("hl", language.clone()),
            ("cselibv", cselibv),
            ("cx", self.cx().to_string()),
            ("q", query.query_text()),
            ("safe", safe.to_string()),
            ("cse_tok", cse_tok),
            ("callback", "_".to_string()),
            ("rurl", String::new()),
            ("searchtype", self.google_categ.clone()),
        ];
        if let Some(seconds) = TimeRange::seconds(query.time_range) {
            let end = chrono::Utc::now();
            let start = end - chrono::Duration::seconds(seconds);
            let sort = format!("date:r:{}:{}", start.format("%Y%m%d"), end.format("%Y%m%d"));
            pairs.push(("sort", sort));
        }
        // Upstream's get_google_info(): `lr` restricts the language, `cr` the
        // country, and only a locale that carries a region sets `cr` / `gl`.
        if locale_language != "all" {
            pairs.push(("lr", format!("lang_{locale_language}")));
        }
        if region != "all" {
            pairs.push(("cr", format!("country{}", region.to_uppercase())));
            pairs.push(("gl", region.to_uppercase()));
        }
        if !exp.is_empty() {
            pairs.push(("exp", exp));
        }
        if pageno > 1 {
            pairs.push(("start", (i64::from(pageno - 1) * page_size).to_string()));
        }
        let url = format!("{ENDPOINT}?{}", query_string(&pairs));
        Ok(EngineRequest::get(url).header("Referer", "https://cse.google.com/"))
    }

    fn parse(
        &self,
        response: &EngineResponse,
        _query: &Query,
    ) -> Result<Vec<EngineItem>, EngineError> {
        // The element endpoint answers with a JSONP callback, not plain JSON.
        let body = response.text();
        let jsonp = body
            .find('{')
            .zip(body.rfind('}'))
            .and_then(|(first, last)| body.get(first..=last))
            .ok_or_else(|| EngineError::Parse("google cse: response has no JSON payload".into()))?;
        let json: serde_json::Value = serde_json::from_str(jsonp)
            .map_err(|error| EngineError::Parse(format!("invalid google cse response: {error}")))?;
        if json.get("error").is_some() {
            if json_path(&json, "error.code").and_then(|value| value.as_i64()) == Some(429) {
                return Err(EngineError::TooManyRequests);
            }
            let message = json_str(&json, "error.message");
            return Err(EngineError::Message(format!("google cse: {message}")));
        }
        let Some(items) = json_path(&json, "results").and_then(|value| value.as_array()) else {
            return Ok(Vec::new());
        };
        let mut results = Vec::new();
        for item in items {
            let mapped = if self.google_categ == "image" {
                image_item(item)
            } else {
                web_item(item)
            };
            if let Some(result) = mapped {
                results.push(result);
            }
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_with(extra: &[(&str, &str)]) -> Arc<GoogleCse> {
        let mut config = EngineConfig::new("google cse", "google_cse", "goc", &["general"]);
        for (key, value) in extra {
            config.extra.insert(
                (*key).to_string(),
                toml::Value::String((*value).to_string()),
            );
        }
        GoogleCse::from_config(&config).expect("google_cse engine")
    }

    fn configured() -> Arc<GoogleCse> {
        engine_with(&[("cse_tok", "tok")])
    }

    fn query(raw: &str) -> Query {
        Query::parse(raw, &crate::engine::EngineRegistry::default(), &[])
    }

    const CSE_JS_FIXTURE: &str = r#"google.cse = google.cse || {}; google.cse.cx = "x";
google.cse.opts = ({"cse_token": "TOKEN123", "cselibVersion": "1.0.0",
  "exp": ["20261101", "20261130"], "cse_api_key": "ignored"});"#;

    fn preflight_response(body: &str) -> EngineResponse {
        EngineResponse::new(200, CSE_JS, body.as_bytes().to_vec())
    }

    #[test]
    fn preflight_is_a_noop_for_unrelated_engines() {
        let config = EngineConfig::new("duckduckgo", "duckduckgo", "ddg", &["general"]);
        let engine: crate::engine::EngineHandle =
            crate::engines::build(&config).expect("duckduckgo engine");
        let query = query("rust");
        assert!(engine.preflight_request(&query, 1).is_none());
        assert!(
            engine
                .preflight(&preflight_response(CSE_JS_FIXTURE), &query)
                .is_ok()
        );
    }

    #[test]
    fn preflight_extracts_the_token_state() {
        let engine = engine_with(&[]);
        assert_eq!(engine.cached_token(), None);
        engine
            .preflight(&preflight_response(CSE_JS_FIXTURE), &query("rust"))
            .expect("token");
        let token = engine.cached_token().expect("cached token");
        assert_eq!(token.cse_tok, "TOKEN123");
        assert_eq!(token.cselibv, "1.0.0");
        assert_eq!(token.exp, "20261101,20261130");
    }

    #[test]
    fn preflight_rejects_a_missing_token() {
        let engine = engine_with(&[]);
        let body = r#"({ "cselibVersion": "1.0.0", "exp": [] });"#;
        assert!(matches!(
            engine.preflight(&preflight_response(body), &query("rust")),
            Err(EngineError::Message(_))
        ));
        assert_eq!(engine.cached_token(), None);
    }

    #[test]
    fn preflight_rejects_a_malformed_payload() {
        let engine = engine_with(&[]);
        for body in [
            "<html>no payload here</html>",
            "({ \"cse_token\": });",
            "}); ({",
        ] {
            assert!(matches!(
                engine.preflight(&preflight_response(body), &query("rust")),
                Err(EngineError::Parse(_))
            ));
        }
        assert_eq!(engine.cached_token(), None);
    }

    #[test]
    fn preflight_reports_http_errors() {
        let engine = engine_with(&[]);
        let response = EngineResponse::new(429, CSE_JS, b"quota".to_vec());
        assert!(matches!(
            engine.preflight(&response, &query("rust")),
            Err(EngineError::TooManyRequests)
        ));
        let response = EngineResponse::new(503, CSE_JS, b"down".to_vec());
        assert!(matches!(
            engine.preflight(&response, &query("rust")),
            Err(EngineError::Message(_))
        ));
    }

    #[test]
    fn token_is_reused_and_refreshed_after_the_ttl() {
        let engine = engine_with(&[]);
        // Cold: one preflight request.
        let preflight = engine
            .preflight_request(&query("rust"), 1)
            .expect("preflight");
        assert_eq!(preflight.url, format!("{CSE_JS}?cx={DEFAULT_CX}"));
        engine
            .preflight(&preflight_response(CSE_JS_FIXTURE), &query("rust"))
            .expect("token");
        // Warm: the cached token is reused, no second preflight.
        assert!(engine.preflight_request(&query("rust"), 1).is_none());
        // Expired: the TTL has passed, so a fresh preflight is requested.
        {
            let mut cached = engine.token.lock().expect("lock");
            *cached = Some(CachedToken {
                token: CseToken {
                    cse_tok: "STALE".into(),
                    cselibv: "old".into(),
                    exp: String::new(),
                },
                expires_at: Instant::now() - Duration::from_secs(1),
            });
        }
        assert_eq!(engine.cached_token(), None);
        assert!(engine.preflight_request(&query("rust"), 1).is_some());
    }

    #[test]
    fn configured_token_overrides_the_preflight() {
        let engine = configured();
        assert!(engine.preflight_request(&query("rust"), 1).is_none());
        let request = engine.build_request(&query("rust"), 1).expect("request");
        assert!(request.url.contains("cse_tok=tok"));
        assert!(request.url.contains("cselibv="));
        assert!(!request.url.contains("exp="));
    }

    #[test]
    fn a_missing_token_is_an_error_not_zero_results() {
        let engine = engine_with(&[]);
        assert!(matches!(
            engine.build_request(&query("rust"), 1),
            Err(EngineError::Message(_))
        ));
    }

    #[test]
    fn final_request_propagates_the_preflight_state() {
        let engine = engine_with(&[]);
        engine
            .preflight(&preflight_response(CSE_JS_FIXTURE), &query("rust"))
            .expect("token");
        let request = engine.build_request(&query("rust"), 1).expect("request");
        assert!(request.url.starts_with(ENDPOINT));
        assert!(request.url.contains("cse_tok=TOKEN123"));
        assert!(request.url.contains("cselibv=1.0.0"));
        assert!(request.url.contains("exp=20261101%2C20261130"));
        assert!(request.url.contains("searchtype="));
        assert!(request.url.contains("callback=_"));
        assert!(request.url.contains("rurl="));
        assert!(request.url.contains("rsz=filtered_cse"));
        assert!(request.url.contains("num=20"));
        assert_eq!(
            request
                .headers
                .get("Referer")
                .and_then(|value| value.to_str().ok()),
            Some("https://cse.google.com/")
        );
    }

    #[test]
    fn image_engine_uses_the_image_category() {
        let engine = engine_with(&[("cse_tok", "tok"), ("google_categ", "image")]);
        let request = engine.build_request(&query("cat"), 1).expect("request");
        assert!(request.url.contains("searchtype=image"));
        let mut config = EngineConfig::new("google cse", "google_cse", "goc", &["general"]);
        config.extra.insert(
            "google_categ".to_string(),
            toml::Value::String("video".into()),
        );
        assert!(GoogleCse::from_config(&config).is_err());
    }

    #[test]
    fn page_two_uses_the_start_offset() {
        let engine = configured();
        let request = engine.build_request(&query("rust"), 2).expect("request");
        assert!(request.url.contains("start=20"));
        let request = engine.build_request(&query("rust"), 3).expect("request");
        assert!(request.url.contains("start=40"));
        let request = engine.build_request(&query("rust"), 1).expect("request");
        assert!(!request.url.contains("start="));
    }

    #[test]
    fn time_ranges_use_the_upstream_windows() {
        let engine = configured();
        for (range, days) in [
            (TimeRange::Day, 1),
            (TimeRange::Week, 7),
            (TimeRange::Month, 30),
            (TimeRange::Year, 365),
        ] {
            let mut search = query("rust");
            search.time_range = Some(range);
            let request = engine.build_request(&search, 1).expect("request");
            let sort = request
                .url
                .split('&')
                .find_map(|pair| pair.strip_prefix("sort="))
                .expect("sort parameter");
            let sort = sort.replace("%3A", ":");
            let window = sort.strip_prefix("date:r:").expect("date window");
            let (start, end) = window.split_once(':').expect("start and end");
            let start = chrono::NaiveDate::parse_from_str(start, "%Y%m%d").expect("start date");
            let end = chrono::NaiveDate::parse_from_str(end, "%Y%m%d").expect("end date");
            assert_eq!((end - start).num_days(), days, "range {range:?}");
        }
    }

    #[test]
    fn locale_sets_language_and_region_parameters() {
        let engine = configured();
        let mut search = query("rust");
        search.locale = "ja-JP".to_string();
        let request = engine.build_request(&search, 1).expect("request");
        assert!(request.url.contains("hl=ja"));
        assert!(request.url.contains("lr=lang_ja"));
        assert!(request.url.contains("cr=countryJP"));
        assert!(request.url.contains("gl=JP"));
        // Without a locale, upstream falls back to `hl=en` and no restriction.
        let request = engine.build_request(&query("rust"), 1).expect("request");
        assert!(request.url.contains("hl=en"));
        assert!(!request.url.contains("lr="));
        assert!(!request.url.contains("cr="));
        assert!(!request.url.contains("gl="));
        // A language without a region only sets the language parameters.
        let mut search = query("rust");
        search.locale = "ja".to_string();
        let request = engine.build_request(&search, 1).expect("request");
        assert!(request.url.contains("hl=ja"));
        assert!(request.url.contains("lr=lang_ja"));
        assert!(!request.url.contains("cr="));
        assert!(!request.url.contains("gl="));
    }

    #[test]
    fn safesearch_maps_to_the_upstream_filter() {
        let engine = configured();
        for (level, expected) in [(0, "off"), (1, "medium"), (2, "high")] {
            let mut search = query("rust");
            search.safesearch = level;
            let request = engine.build_request(&search, 1).expect("request");
            assert!(request.url.contains(&format!("safe={expected}")));
        }
    }

    #[test]
    fn parses_the_web_jsonp_answer() {
        let body = br##"_({"results": [{"unescapedUrl": "https://www.rust-lang.org/", "titleNoFormatting": "Rust", "contentNoFormatting": "A language.", "richSnippet": {"cseThumbnail": {"src": "https://rust-lang.org/l.png"}}}, {"titleNoFormatting": "no url"}]});"##;
        let response = EngineResponse::new(200, ENDPOINT, body.to_vec());
        let items = configured()
            .parse(&response, &query("rust"))
            .expect("items");
        assert_eq!(items.len(), 1);
        match &items[0] {
            EngineItem::Result(result) => {
                assert_eq!(result.url, "https://www.rust-lang.org/");
                assert_eq!(result.title, "Rust");
                assert!(result.content.starts_with("A language"));
                assert_eq!(
                    result.thumbnail.as_deref(),
                    Some("https://rust-lang.org/l.png")
                );
                assert!(result.img_src.is_none());
            }
            _ => panic!("expected a result"),
        }
    }

    #[test]
    fn parses_the_image_jsonp_answer() {
        let body = br##"_({"results": [{"originalContextUrl": "https://example.com/cats", "unescapedUrl": "https://example.com/cat.jpg", "tbUrl": "https://example.com/cat_tb.jpg", "titleNoFormatting": "A cat", "contentNoFormatting": "Photo", "width": "1200", "height": "800", "fileFormat": "image/jpeg"}, {"unescapedUrl": "https://example.com/no-context.jpg"}]});"##;
        let response = EngineResponse::new(200, ENDPOINT, body.to_vec());
        let engine = engine_with(&[("cse_tok", "tok"), ("google_categ", "image")]);
        let items = engine.parse(&response, &query("cat")).expect("items");
        assert_eq!(items.len(), 1);
        match &items[0] {
            EngineItem::Result(result) => {
                assert_eq!(result.url, "https://example.com/cats");
                assert_eq!(result.title, "A cat");
                assert_eq!(result.content, "Photo");
                assert_eq!(
                    result.img_src.as_deref(),
                    Some("https://example.com/cat.jpg")
                );
                assert_eq!(
                    result.thumbnail.as_deref(),
                    Some("https://example.com/cat_tb.jpg")
                );
                assert_eq!(result.template, Template::Images);
                assert_eq!(
                    result.metadata.get("resolution").map(String::as_str),
                    Some("1200 x 800")
                );
                assert_eq!(
                    result.metadata.get("img_format").map(String::as_str),
                    Some("jpeg")
                );
            }
            _ => panic!("expected an image result"),
        }
    }

    #[test]
    fn reports_api_errors_and_malformed_answers() {
        let body = br##"_({"error": {"code": 429, "message": "quota"}});"##;
        let response = EngineResponse::new(200, ENDPOINT, body.to_vec());
        assert!(matches!(
            configured().parse(&response, &query("rust")),
            Err(EngineError::TooManyRequests)
        ));
        let body = br##"_({"error": {"code": 400, "message": "bad query"}});"##;
        let response = EngineResponse::new(200, ENDPOINT, body.to_vec());
        assert!(matches!(
            configured().parse(&response, &query("rust")),
            Err(EngineError::Message(_))
        ));
        let response = EngineResponse::new(200, ENDPOINT, b"not json".to_vec());
        assert!(matches!(
            configured().parse(&response, &query("rust")),
            Err(EngineError::Parse(_))
        ));
        let response = EngineResponse::new(200, ENDPOINT, b"_({});".to_vec());
        assert!(
            configured()
                .parse(&response, &query("rust"))
                .unwrap()
                .is_empty()
        );
    }
}
