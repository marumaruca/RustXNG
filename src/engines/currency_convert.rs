//! Currency conversion engine, ported from searx/engines/currency_convert.py.
//!
//! Upstream is driven by the online_currency query processor; the query type of
//! this port only carries the search terms, so the syntax is parsed here. The
//! answer uses ISO codes where upstream prints the currency names.
// engine-id: currency_convert

use std::sync::Arc;

use regex::Regex;

use crate::engine::util::*;
use crate::engine::{
    Engine, EngineDefaults, EngineError, EngineItem, EngineMeta, EngineRequest, EngineResponse,
};
use crate::query::Query;
use crate::settings::EngineConfig;

const DEFAULTS: EngineDefaults = EngineDefaults {
    categories: &["currency", "general"],
    shortcut: "cc",
    paging: false,
    time_range_support: false,
    safesearch: false,
    language_support: false,
    weight: 100.0,
    timeout: 4.0,
};

const BASE_URL: &str = "https://duckduckgo.com/js/spice/currency";

/// ISO 4217 codes with the names accepted by the query syntax.
const CURRENCIES: &[(&str, &[&str])] = &[
    ("AUD", &["aud", "australian dollar"]),
    ("CAD", &["cad", "canadian dollar"]),
    ("CHF", &["chf", "swiss franc", "franc"]),
    ("CNY", &["cny", "chinese yuan", "yuan", "renminbi"]),
    ("EUR", &["eur", "euro", "euros"]),
    (
        "GBP",
        &["gbp", "british pound", "pound", "pounds", "sterling"],
    ),
    ("INR", &["inr", "indian rupee", "rupee", "rupees"]),
    ("JPY", &["jpy", "japanese yen", "yen"]),
    ("KRW", &["krw", "south korean won", "won"]),
    ("MXN", &["mxn", "mexican peso", "peso", "pesos"]),
    ("NZD", &["nzd", "new zealand dollar"]),
    ("RUB", &["rub", "russian ruble", "ruble", "rouble"]),
    ("SEK", &["sek", "swedish krona", "krona"]),
    ("SGD", &["sgd", "singapore dollar"]),
    ("TRY", &["try", "turkish lira", "lira"]),
    ("USD", &["usd", "us dollar", "dollar", "dollars", "$"]),
    ("ZAR", &["zar", "south african rand", "rand"]),
];

struct Conversion {
    amount: f64,
    from: String,
    to: String,
}

/// Currency conversion through DuckDuckGo's spice endpoint.
pub struct CurrencyConvert {
    meta: EngineMeta,
}

impl CurrencyConvert {
    pub fn from_config(config: &EngineConfig) -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            meta: EngineMeta::from_config(config, DEFAULTS),
        }))
    }
}

fn iso4217(raw: &str) -> Option<String> {
    let mut name = raw.trim().to_lowercase().replace('-', " ");
    while name.contains("  ") {
        name = name.replace("  ", " ");
    }
    let name = name.trim();
    for &(code, names) in CURRENCIES {
        if code.eq_ignore_ascii_case(name) || names.contains(&name) {
            return Some(code.to_string());
        }
    }
    None
}
fn conversion(text: &str) -> Option<Conversion> {
    let pattern = Regex::new(r"(?i)(\d+(?:\.\d+)?) ([^.0-9]+) (?:in|to) ([^.0-9]+)").ok()?;
    let captures = pattern.captures(text)?;
    let amount = captures.get(1)?.as_str().parse::<f64>().ok()?;
    let from = iso4217(captures.get(2)?.as_str())?;
    let to = iso4217(captures.get(3)?.as_str())?;
    Some(Conversion { amount, from, to })
}
fn request_url(meta: &EngineMeta, conversion: &Conversion) -> String {
    let base = meta
        .base_url
        .as_deref()
        .unwrap_or(BASE_URL)
        .trim_end_matches('/');
    format!("{base}/1/{}/{}", conversion.from, conversion.to)
}
fn number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}
fn json_slice(body: &str) -> Option<&str> {
    let start = body.find('{')?;
    let end = body.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&body[start..=end])
}

impl Engine for CurrencyConvert {
    fn meta(&self) -> &EngineMeta {
        &self.meta
    }

    fn is_applicable(&self, query: &Query) -> bool {
        conversion(&query.query_text()).is_some()
    }

    fn build_request(&self, query: &Query, _pageno: u32) -> Result<EngineRequest, EngineError> {
        let Some(conversion) = conversion(&query.query_text()) else {
            return Err(EngineError::Message(
                "currency_convert expects a query like 10 usd to eur".to_string(),
            ));
        };
        Ok(EngineRequest::get(request_url(&self.meta, &conversion)))
    }

    fn parse(
        &self,
        response: &EngineResponse,
        query: &Query,
    ) -> Result<Vec<EngineItem>, EngineError> {
        let Some(conversion) = conversion(&query.query_text()) else {
            return Ok(Vec::new());
        };
        let body = response.text();
        let Some(slice) = json_slice(&body) else {
            return Ok(Vec::new());
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(slice) else {
            return Ok(Vec::new());
        };
        let Some(mid) = json_path(&json, "to.0.mid") else {
            return Ok(Vec::new());
        };
        let rate = mid
            .as_f64()
            .or_else(|| mid.as_str().and_then(|text| text.parse::<f64>().ok()));
        let Some(rate) = rate else {
            return Ok(Vec::new());
        };
        let answer = format!(
            "{} {} = {} {} (1 {} : {} {})",
            number(conversion.amount),
            conversion.from,
            number(conversion.amount * rate),
            conversion.to,
            conversion.from,
            number(rate),
            conversion.to
        );
        Ok(vec![EngineItem::Answer(answer)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Arc<CurrencyConvert> {
        CurrencyConvert::from_config(&EngineConfig::new(
            "currency",
            "currency_convert",
            "cc",
            &["currency", "general"],
        ))
        .expect("currency engine")
    }

    fn query(text: &str) -> Query {
        Query::parse(text, &crate::engine::EngineRegistry::default(), &[])
    }

    fn response(body: &str) -> EngineResponse {
        EngineResponse::new(200, "https://duckduckgo.com", body.as_bytes().to_vec())
    }

    #[test]
    fn builds_the_spice_url() {
        assert_eq!(
            engine()
                .build_request(&query("10 usd to eur"), 1)
                .unwrap()
                .url,
            "https://duckduckgo.com/js/spice/currency/1/USD/EUR"
        );
        assert_eq!(
            engine()
                .build_request(&query("5 dollars in euros"), 1)
                .unwrap()
                .url,
            "https://duckduckgo.com/js/spice/currency/1/USD/EUR"
        );
    }

    #[test]
    fn an_unrelated_query_is_rejected() {
        assert!(engine().build_request(&query("bitcoin"), 1).is_err());
    }

    #[test]
    fn applicability_matches_the_conversion_syntax() {
        assert!(engine().is_applicable(&query("10 usd to eur")));
        assert!(engine().is_applicable(&query("5 dollars in euros")));
        assert!(!engine().is_applicable(&query("bitcoin")));
        assert!(!engine().is_applicable(&query("tokyo")));
    }

    #[test]
    fn answers_the_conversion() {
        let body = "ddg_spice_currency(\n{\"to\":[{\"mid\":\"0.5\"}]}\n);";
        let items = engine()
            .parse(&response(body), &query("10 usd to eur"))
            .unwrap();
        assert_eq!(items.len(), 1);
        match &items[0] {
            EngineItem::Answer(answer) => {
                assert_eq!(answer, "10.0 USD = 5.0 EUR (1 USD : 0.5 EUR)")
            }
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    #[test]
    fn a_response_without_a_rate_is_empty() {
        let items = engine()
            .parse(
                &response("ddg_spice_currency({});"),
                &query("10 usd to eur"),
            )
            .unwrap();
        assert!(items.is_empty());
    }
}
