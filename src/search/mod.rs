use std::cmp::{max, min};

use time::format_description;
use time::{Date, Duration, Time};

#[derive(Debug, Clone, Default)]
pub struct RangeFilter {
    pub from: Option<i64>,
    pub to: Option<i64>, // exclusive
}

impl RangeFilter {
    pub fn has_range(&self) -> bool {
        self.from.is_some() || self.to.is_some()
    }

    pub fn merge(&mut self, other: RangeFilter) {
        if let Some(from) = other.from {
            self.from = Some(match self.from {
                Some(existing) => max(existing, from),
                None => from,
            });
        }
        if let Some(to) = other.to {
            self.to = Some(match self.to {
                Some(existing) => min(existing, to),
                None => to,
            });
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub terms: Vec<String>,
    pub title_terms: Vec<String>,
    pub tags: Vec<String>,
    pub created: RangeFilter,
    pub updated: RangeFilter,
    pub regex_pattern: Option<String>,
}

impl SearchQuery {
    pub fn has_terms(&self) -> bool {
        !self.terms.is_empty() || !self.title_terms.is_empty()
    }

    pub fn has_filters(&self) -> bool {
        !self.tags.is_empty() || self.created.has_range() || self.updated.has_range()
    }

    pub fn highlight_terms(&self) -> Vec<String> {
        let mut terms = self.terms.clone();
        terms.extend(self.title_terms.iter().cloned());
        terms
    }
}

pub fn parse_query(input: &str) -> SearchQuery {
    let mut query = SearchQuery::default();
    for raw in input.split_whitespace() {
        if raw.is_empty() {
            continue;
        }
        if let Some(tag) = raw.strip_prefix("tag:") {
            if let Some(value) = sanitize_term(tag) {
                query.tags.push(value.to_lowercase());
            }
            continue;
        }
        if let Some(term) = raw.strip_prefix("title:") {
            if let Some(value) = sanitize_term(term) {
                query.title_terms.push(value);
            }
            continue;
        }
        if let Some(range) = raw.strip_prefix("created:") {
            let parsed = parse_date_range(range);
            query.created.merge(parsed);
            continue;
        }
        if let Some(range) = raw.strip_prefix("updated:") {
            let parsed = parse_date_range(range);
            query.updated.merge(parsed);
            continue;
        }
        if let Some(value) = sanitize_term(raw) {
            query.terms.push(value);
        }
    }
    query
}

pub fn regex_pattern_from_input(input: &str) -> Option<String> {
    let mut parts = Vec::new();
    for raw in input.split_whitespace() {
        if raw.starts_with("tag:")
            || raw.starts_with("title:")
            || raw.starts_with("created:")
            || raw.starts_with("updated:")
        {
            continue;
        }
        parts.push(raw.to_string());
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn sanitize_term(raw: &str) -> Option<String> {
    let term: String = raw
        .chars()
        .filter(|ch| ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/'))
        .collect();
    if term.is_empty() {
        None
    } else {
        Some(term)
    }
}

fn parse_date_range(spec: &str) -> RangeFilter {
    let mut range = RangeFilter::default();
    let parts: Vec<&str> = spec.split("..").collect();
    match parts.as_slice() {
        [single] => {
            if let Some((from, to)) = parse_single_date(single) {
                range.from = Some(from);
                range.to = Some(to);
            }
        }
        [from, to] => {
            if !from.is_empty() {
                if let Some((start, _)) = parse_single_date(from) {
                    range.from = Some(start);
                }
            }
            if !to.is_empty() {
                if let Some((_, end)) = parse_single_date(to) {
                    range.to = Some(end);
                }
            }
        }
        _ => {}
    }
    range
}

fn parse_single_date(input: &str) -> Option<(i64, i64)> {
    static FORMAT: once_cell::sync::Lazy<Vec<format_description::FormatItem<'static>>> =
        once_cell::sync::Lazy::new(|| {
            format_description::parse("[year]-[month]-[day]")
                .expect("valid date format description")
        });
    let date = Date::parse(input, &*FORMAT).ok()?;
    let from = date.with_time(Time::MIDNIGHT).assume_utc().unix_timestamp();
    let to = date
        .checked_add(Duration::days(1))?
        .with_time(Time::MIDNIGHT)
        .assume_utc()
        .unix_timestamp();
    Some((from, to))
}
