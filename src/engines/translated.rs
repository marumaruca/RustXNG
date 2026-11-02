//! MyMemory Translated engine, ported from searx/engines/translated.py.
//!
//! Upstream is an online dictionary engine: the source and target languages
//! come from the prefix of the query (for example en-de hello). The port
//! derives the same dictionary parameters from the query text.
// engine-id: translated

use std::sync::Arc;

use crate::engine::util::*;
use crate::engine::{
    Engine, EngineDefaults, EngineError, EngineItem, EngineMeta, EngineRequest, EngineResponse,
};
use crate::query::Query;
use crate::settings::EngineConfig;

const DEFAULTS: EngineDefaults = EngineDefaults {
    categories: &["general", "translate"],
    shortcut: "tl",
    paging: false,
    time_range_support: false,
    safesearch: false,
    language_support: false,
    weight: 100.0,
    timeout: 5.0,
};

const API_URL: &str = "https://api.mymemory.translated.net";
const WEB_URL: &str = "https://mymemory.translated.net";

/// The language pair and the text to translate.
struct Direction {
    from: String,
    to: String,
    text: String,
}

/// MyMemory, through its official translation API.
pub struct Translated {
    meta: EngineMeta,
}

impl Translated {
    pub fn from_config(config: &EngineConfig) -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            meta: EngineMeta::from_config(config, DEFAULTS),
        }))
    }

    fn api_url(&self) -> String {
        self.meta
            .base_url
            .as_deref()
            .unwrap_or(API_URL)
            .trim_end_matches('/')
            .to_string()
    }
}

fn is_http_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Upstream search syntax: a from-to pair followed by the text.
fn direction(query: &Query) -> Option<Direction> {
    online_dictionary_direction(&query.query_text()).map(|(from, to, text)| Direction {
        from,
        to,
        text,
    })
}

impl Engine for Translated {
    fn meta(&self) -> &EngineMeta {
        &self.meta
    }

    fn is_applicable(&self, query: &Query) -> bool {
        direction(query).is_some()
    }

    fn build_request(&self, query: &Query, _pageno: u32) -> Result<EngineRequest, EngineError> {
        let Some(direction) = direction(query) else {
            return Err(EngineError::Message(
                "translated expects a query like en-de hello".to_string(),
            ));
        };
        let mut pairs: Vec<(&str, String)> = vec![
            ("q", direction.text.clone()),
            ("langpair", format!("{}|{}", direction.from, direction.to)),
        ];
        let key = self
            .meta
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty());
        if let Some(key) = key {
            pairs.push(("key", key.to_string()));
        }
        let url = format!("{}/get?{}", self.api_url(), query_string(&pairs));
        Ok(EngineRequest::get(url).header("Accept", "application/json"))
    }

    fn parse(
        &self,
        response: &EngineResponse,
        query: &Query,
    ) -> Result<Vec<EngineItem>, EngineError> {
        let Some(direction) = direction(query) else {
            return Ok(Vec::new());
        };
        let json = response.json()?;
        let translated = strip_tags(&json_str(&json, "responseData.translatedText"));
        if translated.is_empty() {
            return Ok(Vec::new());
        }
        let mut examples: Vec<String> = Vec::new();
        let matches = json_path(&json, "matches").and_then(serde_json::Value::as_array);
        for entry in matches.into_iter().flatten() {
            let translation = strip_tags(&json_str(entry, "translation"));
            let segment = strip_tags(&json_str(entry, "segment"));
            if translation.is_empty() || translation == translated || segment.is_empty() {
                continue;
            }
            let example = format!("{segment} : {translation}");
            if !examples.contains(&example) {
                examples.push(example);
            }
        }
        let locale = if query.locale.trim().is_empty() {
            "en"
        } else {
            query.locale.as_str()
        };
        let args: Vec<(&str, String)> = vec![
            ("q", direction.text.clone()),
            ("lang", locale.to_string()),
            ("sl", direction.from.clone()),
            ("tl", direction.to.clone()),
        ];
        let link = format!("{WEB_URL}/search.php?{}", query_string(&args));
        if !is_http_url(&link) {
            return Ok(Vec::new());
        }
        let content = std::iter::once(translated)
            .chain(examples)
            .collect::<Vec<String>>()
            .join(" ");
        Ok(vec![
            ResultBuilder::new(link, direction.text)
                .content(content)
                .metadata("from", direction.from)
                .metadata("to", direction.to)
                .build(),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r##"{
      "responseData": {"translatedText": "Hallo Welt"},
      "matches": [
        {"segment": "Hello world", "translation": "Hallo Welt"},
        {"segment": "Hello", "translation": "Hallo"},
        {"segment": "hi", "translation": "Hallo Welt"}
      ]
    }"##;

    fn engine() -> Arc<Translated> {
        let config = EngineConfig::new("mymemory translated", "translated", "tl", &["general"]);
        Translated::from_config(&config).expect("translated engine")
    }

    fn query(source: &str) -> Query {
        Query::parse(source, &crate::engine::EngineRegistry::default(), &[])
    }

    #[test]
    fn parses_a_translation_with_examples() {
        let response = EngineResponse::new(200, API_URL, FIXTURE.as_bytes().to_vec());
        let items = engine()
            .parse(&response, &query("en-de hello world"))
            .unwrap();
        assert_eq!(items.len(), 1);
        match &items[0] {
            EngineItem::Result(result) => {
                assert_eq!(result.title, "hello world");
                assert_eq!(
                    result.url,
                    "https://mymemory.translated.net/search.php?q=hello%20world&lang=en&sl=en&tl=de"
                );
                assert_eq!(result.content, "Hallo Welt Hello : Hallo");
                assert_eq!(result.metadata.get("from").map(String::as_str), Some("en"));
                assert_eq!(result.metadata.get("to").map(String::as_str), Some("de"));
            }
            other => panic!("expected a result, got {other:?}"),
        }
    }

    #[test]
    fn builds_the_api_url_with_the_language_pair() {
        let request = engine()
            .build_request(&query("en-de hello world"), 1)
            .unwrap();
        assert_eq!(
            request.url,
            "https://api.mymemory.translated.net/get?q=hello%20world&langpair=en%7Cde"
        );
        assert!(engine().build_request(&query("hello"), 1).is_err());
    }

    #[test]
    fn an_empty_response_is_empty() {
        let response = EngineResponse::new(200, API_URL, b"{}".to_vec());
        assert!(
            engine()
                .parse(&response, &query("en-de hi"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn applicability_requires_the_language_pair_prefix() {
        assert!(engine().is_applicable(&query("en-de hello world")));
        assert!(engine().is_applicable(&query("english-german hallo welt")));
        assert!(!engine().is_applicable(&query("hello")));
        assert!(!engine().is_applicable(&query("en-de")));
        assert!(!engine().is_applicable(&query("xx-yy hello")));
    }
}
