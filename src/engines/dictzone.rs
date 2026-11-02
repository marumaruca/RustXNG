//! Dictzone engine, ported from searx/engines/dictzone.py.
//!
//! Dictzone is an offline-capable online dictionary; upstream scrapes the
//! two-column translation table.

use std::sync::Arc;

use crate::engine::util::*;
use crate::engine::{
    Engine, EngineDefaults, EngineError, EngineItem, EngineMeta, EngineRequest, EngineResponse,
};
use crate::query::Query;
use crate::settings::EngineConfig;

// engine-id: dictzone

const BASE_URL: &str = "https://dictzone.com";

const DEFAULTS: EngineDefaults = EngineDefaults {
    categories: &["general", "translate"],
    shortcut: "",
    paging: false,
    time_range_support: false,
    safesearch: false,
    language_support: true,
    weight: 1.0,
    timeout: 4.0,
};

/// Dictzone translation search.
pub struct Dictzone {
    meta: EngineMeta,
}

impl Dictzone {
    pub fn from_config(config: &EngineConfig) -> Result<Arc<Self>, String> {
        let meta = EngineMeta::from_config(config, DEFAULTS);
        Ok(Arc::new(Self { meta }))
    }

    #[test]
    fn applicability_requires_the_language_pair_prefix() {
        assert!(engine().is_applicable(&query("en-de hello")));
        assert!(engine().is_applicable(&query("english-german hallo")));
        assert!(!engine().is_applicable(&query("hello world")));
        assert!(!engine().is_applicable(&query("en-de")));
        assert!(!engine().is_applicable(&query("xx-yy hello")));
    }
}

fn direction(query: &Query) -> Option<(String, String, String)> {
    online_dictionary_direction(&query.query_text())
}

fn language_name(language: &str) -> String {
    crate::locales::LOCALES
        .iter()
        .find(|locale| locale.id == language)
        .map(|locale| locale.english.to_lowercase())
        .unwrap_or_else(|| language.to_string())
}

impl Engine for Dictzone {
    fn meta(&self) -> &EngineMeta {
        &self.meta
    }

    fn is_applicable(&self, query: &Query) -> bool {
        direction(query).is_some()
    }

    fn build_request(&self, query: &Query, _pageno: u32) -> Result<EngineRequest, EngineError> {
        let Some((from, to, text)) = direction(query) else {
            return Err(EngineError::Message(
                "dictzone expects a query like en-de hello".to_string(),
            ));
        };
        let base = self.meta.base_url.as_deref().unwrap_or(BASE_URL);
        Ok(EngineRequest::get(format!(
            "{base}/{}-{}-dictionary/{}",
            language_name(&from),
            language_name(&to),
            encode(&text)
        )))
    }

    fn parse(
        &self,
        response: &EngineResponse,
        query: &Query,
    ) -> Result<Vec<EngineItem>, EngineError> {
        if direction(query).is_none() {
            return Ok(Vec::new());
        }
        let html = parse_html(&response.text());
        let url = self.build_request(query, 1)?.url;
        let mut results = Vec::new();
        for row in select_elements(&html, "table#r tr")? {
            let cells = children(row, "td");
            if cells.len() != 2 {
                continue;
            }
            let source = text(cells[0]);
            if source.is_empty() {
                continue;
            }
            let mut content = source.clone();
            let mut first = true;
            for paragraph in children(cells[1], "p") {
                let mut line = text(paragraph);
                if let Some(sample) = child(paragraph, "i.smpl") {
                    let sample = text(sample);
                    if !sample.is_empty() {
                        line = format!("{line} // {sample}");
                    }
                }
                if first {
                    content = format!("{source} : {line}");
                    first = false;
                }
            }
            results.push(
                ResultBuilder::new(url.clone(), source)
                    .content(content)
                    .build(),
            );
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Arc<Dictzone> {
        Dictzone::from_config(&EngineConfig::new("dictzone", "dictzone", "", &["general"]))
            .expect("engine")
    }

    fn query(text: &str) -> Query {
        Query::parse(text, &crate::engine::EngineRegistry::default(), &[])
    }

    #[test]
    fn builds_the_dictionary_url_from_the_query_prefix() {
        let mut q = query("en-de hello");
        q.locale = "ja-JP".to_string();
        let request = engine().build_request(&q, 1).unwrap();
        assert_eq!(
            request.url,
            "https://dictzone.com/english-german-dictionary/hello"
        );
        assert_eq!(
            engine()
                .build_request(&query("english-german hallo"), 1)
                .unwrap()
                .url,
            "https://dictzone.com/english-german-dictionary/hallo"
        );
        assert!(engine().build_request(&query("hello"), 1).is_err());
    }

    #[test]
    fn parses_the_translation_rows() {
        let html = r##"<table id="r">
            <tr><th>English</th><th>German</th></tr>
            <tr><td><i>hello</i> world</td><td><p>hallo</p><p>servus</p></td></tr>
        </table>"##;
        let response = EngineResponse::new(200, BASE_URL, html.as_bytes().to_vec());
        let items = engine()
            .parse(&response, &query("en-de hello world"))
            .unwrap();
        assert_eq!(items.len(), 1);
        match &items[0] {
            EngineItem::Result(result) => {
                assert_eq!(result.title, "hello world");
                assert!(result.content.contains("hallo"));
                assert_eq!(
                    result.url,
                    "https://dictzone.com/english-german-dictionary/hello%20world"
                );
            }
            _ => panic!("expected a result"),
        }
    }
}
