//! Wikidata engine, ported from `searx/engines/wikidata.py`.
//!
//! The top entity hit becomes a bounded infobox instead of ranked main
//! results: entity hits are reference material, and mixing them into the
//! ordinary result list skews the ranking.

use std::sync::Arc;

use crate::engine::util::*;
use crate::engine::{
    Engine, EngineDefaults, EngineError, EngineItem, EngineMeta, EngineRequest, EngineResponse,
};
use crate::query::Query;
use crate::results::{Infobox, InfoboxUrl};
use crate::settings::EngineConfig;

const DEFAULTS: EngineDefaults = EngineDefaults {
    categories: &["general"],
    shortcut: "wd",
    paging: false,
    time_range_support: false,
    safesearch: false,
    language_support: true,
    weight: 1.0,
    timeout: 4.0,
};

const API_URL: &str = "https://www.wikidata.org/w/api.php";

/// Wikidata entity search.
pub struct Wikidata {
    meta: EngineMeta,
}

impl Wikidata {
    pub fn from_config(config: &EngineConfig) -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            meta: EngineMeta::from_config(config, DEFAULTS),
        }))
    }
}

impl Engine for Wikidata {
    fn meta(&self) -> &EngineMeta {
        &self.meta
    }

    fn build_request(&self, query: &Query, _pageno: u32) -> Result<EngineRequest, EngineError> {
        let language = locale_language(&query.locale);
        let language = if language == "all" {
            "en".to_string()
        } else {
            language
        };
        let pairs: Vec<(&str, String)> = vec![
            ("action", "wbsearchentities".to_string()),
            ("search", query.query_text()),
            ("language", language.clone()),
            ("uselang", language),
            ("limit", "10".to_string()),
            ("format", "json".to_string()),
        ];
        Ok(
            EngineRequest::get(format!("{API_URL}?{}", query_string(&pairs)))
                .header("Accept", "application/json"),
        )
    }

    fn parse(
        &self,
        response: &EngineResponse,
        _query: &Query,
    ) -> Result<Vec<EngineItem>, EngineError> {
        let json = response.json()?;
        let items = match json.get("search").and_then(|value| value.as_array()) {
            Some(items) => items,
            None => return Ok(Vec::new()),
        };
        // One bounded infobox from the top hit; no ranked main results.
        let Some(item) = items.first() else {
            return Ok(Vec::new());
        };
        let id = json_str(item, "id");
        if id.is_empty() {
            return Ok(Vec::new());
        }
        let title = {
            let label = json_str(item, "label");
            if label.is_empty() { id.clone() } else { label }
        };
        let url = format!("https://www.wikidata.org/wiki/{id}");
        Ok(vec![EngineItem::Infobox(Infobox {
            id: "wikidata".to_string(),
            title: title.clone(),
            content: json_str(item, "description"),
            img_src: None,
            url: Some(url.clone()),
            urls: vec![InfoboxUrl {
                title: title.clone(),
                url,
            }],
            ..Default::default()
        })])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r##"{
      "search": [
        {"id": "Q1344", "label": "Tokyo", "description": "capital of Japan"},
        {"id": "Q205769", "label": "Tokyo Tower", "description": "tower in Minato, Tokyo"}
      ]
    }"##;

    fn engine() -> Arc<Wikidata> {
        Wikidata::from_config(&EngineConfig::new(
            "wikidata",
            "wikidata",
            "wd",
            &["general"],
        ))
        .expect("wikidata engine")
    }

    #[test]
    fn parses_a_bounded_infobox_from_the_top_hit() {
        let response = EngineResponse::new(200, API_URL, FIXTURE.as_bytes().to_vec());
        let query = Query::parse("tokyo", &crate::engine::EngineRegistry::default(), &[]);
        let items = engine().parse(&response, &query).unwrap();
        assert_eq!(items.len(), 1);
        match &items[0] {
            EngineItem::Infobox(infobox) => {
                assert_eq!(infobox.id, "wikidata");
                assert_eq!(infobox.title, "Tokyo");
                assert_eq!(infobox.content, "capital of Japan");
                assert_eq!(
                    infobox.url.as_deref(),
                    Some("https://www.wikidata.org/wiki/Q1344")
                );
                assert_eq!(infobox.urls.len(), 1);
                assert_eq!(infobox.urls[0].url, "https://www.wikidata.org/wiki/Q1344");
            }
            other => panic!("expected an infobox, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_search_is_empty() {
        let response = EngineResponse::new(200, API_URL, b"{}".to_vec());
        let query = Query::parse("tokyo", &crate::engine::EngineRegistry::default(), &[]);
        assert!(engine().parse(&response, &query).unwrap().is_empty());
    }
}
