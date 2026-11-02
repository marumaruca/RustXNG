//! Wikipedia engine, ported from `searx/engines/wikipedia.py`.
//!
//! The MediaWiki search API is used. The top hit becomes a bounded infobox
//! instead of ranked main results: encyclopedia hits are reference material,
//! and mixing them into the ordinary result list skews the ranking.

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
    shortcut: "wp",
    paging: true,
    time_range_support: false,
    safesearch: false,
    language_support: true,
    weight: 1.0,
    timeout: 4.0,
};

/// Wikipedia.
pub struct Wikipedia {
    meta: EngineMeta,
}

impl Wikipedia {
    pub fn from_config(config: &EngineConfig) -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            meta: EngineMeta::from_config(config, DEFAULTS),
        }))
    }

    /// The wiki host for a language, e.g. `ja.wikipedia.org`.
    fn host(&self, query: &Query) -> String {
        if let Some(base) = &self.meta.base_url {
            return base.trim_end_matches('/').to_string();
        }
        let language = locale_language(&query.locale);
        let language = if language == "all" {
            "en".to_string()
        } else {
            language
        };
        format!("https://{language}.wikipedia.org")
    }
}

impl Engine for Wikipedia {
    fn meta(&self) -> &EngineMeta {
        &self.meta
    }

    fn build_request(&self, query: &Query, pageno: u32) -> Result<EngineRequest, EngineError> {
        let pairs: Vec<(&str, String)> = vec![
            ("action", "query".to_string()),
            ("list", "search".to_string()),
            ("srsearch", query.query_text()),
            ("srlimit", "10".to_string()),
            ("sroffset", ((pageno - 1) * 10).to_string()),
            ("format", "json".to_string()),
        ];
        Ok(EngineRequest::get(format!(
            "{}/w/api.php?{}",
            self.host(query),
            query_string(&pairs)
        ))
        .header("Accept", "application/json"))
    }

    fn parse(
        &self,
        response: &EngineResponse,
        query: &Query,
    ) -> Result<Vec<EngineItem>, EngineError> {
        let json = response.json()?;
        let items = match json_path(&json, "query.search").and_then(|value| value.as_array()) {
            Some(items) => items,
            None => return Ok(Vec::new()),
        };
        // One bounded infobox from the top hit; no ranked main results.
        let Some(item) = items.first() else {
            return Ok(Vec::new());
        };
        let title = json_str(item, "title");
        if title.is_empty() {
            return Ok(Vec::new());
        }
        let host = self.host(query);
        let page_id = json_str(item, "pageid");
        let url = if page_id.is_empty() {
            format!("{host}/wiki/{}", encode(&title.replace(' ', "_")))
        } else {
            format!("{host}/?curid={page_id}")
        };
        Ok(vec![EngineItem::Infobox(Infobox {
            id: "wikipedia".to_string(),
            title: title.clone(),
            content: strip_tags(&json_str(item, "snippet")),
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
      "query": {"search": [
        {"title": "Tokyo", "pageid": "20367", "snippet": "<span class=\"searchmatch\">Tokyo</span> is the capital."},
        {"title": "Tokyo Tower", "pageid": "14598", "snippet": "A tower in Tokyo."}
      ]}
    }"##;

    fn engine() -> Arc<Wikipedia> {
        Wikipedia::from_config(&EngineConfig::new(
            "wikipedia",
            "wikipedia",
            "wp",
            &["general"],
        ))
        .expect("wikipedia engine")
    }

    #[test]
    fn parses_a_bounded_infobox_from_the_top_hit() {
        let response =
            EngineResponse::new(200, "https://en.wikipedia.org", FIXTURE.as_bytes().to_vec());
        let query = Query::parse("tokyo", &crate::engine::EngineRegistry::default(), &[]);
        let items = engine().parse(&response, &query).unwrap();
        assert_eq!(items.len(), 1);
        match &items[0] {
            EngineItem::Infobox(infobox) => {
                assert_eq!(infobox.id, "wikipedia");
                assert_eq!(infobox.title, "Tokyo");
                assert!(infobox.content.contains("capital"));
                assert_eq!(
                    infobox.url.as_deref(),
                    Some("https://en.wikipedia.org/?curid=20367")
                );
                assert_eq!(infobox.urls.len(), 1);
                assert_eq!(infobox.urls[0].url, "https://en.wikipedia.org/?curid=20367");
            }
            other => panic!("expected an infobox, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_search_is_empty() {
        let response =
            EngineResponse::new(200, "https://en.wikipedia.org", b"{\"query\":{}}".to_vec());
        let query = Query::parse("tokyo", &crate::engine::EngineRegistry::default(), &[]);
        assert!(engine().parse(&response, &query).unwrap().is_empty());
    }
}
