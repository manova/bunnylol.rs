/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashSet;
use url::Url;

/// An immutable, validated identifier rule. Regexes and templates are compiled
/// during deserialization, rather than for each CLI command or HTTP request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "PatternBindingConfig")]
pub struct PatternBinding {
    name: String,
    pattern: String,
    url: String,
    description: Option<String>,
    #[serde(skip)]
    regex: Regex,
    #[serde(skip)]
    parts: Vec<TemplatePart>,
    #[serde(skip)]
    destination: Url,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PatternBindingConfig {
    name: String,
    pattern: String,
    url: String,
    description: Option<String>,
}

#[derive(Debug, Clone)]
enum TemplatePart {
    Literal(String),
    Capture(String),
}

// Keep common identifier punctuation readable, while encoding every byte that
// could introduce URL structure. Dots stay encoded as an additional precaution.
const CAPTURE_ENCODING: &percent_encoding::AsciiSet =
    &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'~');

// RFC 3986 reserved and unreserved characters are safe in a serialized URI.
// Keep '%' so already encoded capture bytes are not encoded a second time.
const URI_ENCODING: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'<')
    .add(b'>')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'|')
    .add(b'^')
    .add(b'\\');
const FRAGMENT_ENCODING: &percent_encoding::AsciiSet = &URI_ENCODING.add(b'#');

fn has_dot_segment(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.split('/').any(|segment| {
        percent_decode_str(segment)
            .decode_utf8()
            .is_ok_and(|value| value == "." || value == "..")
    })
}

impl TryFrom<PatternBindingConfig> for PatternBinding {
    type Error = String;

    fn try_from(raw: PatternBindingConfig) -> Result<Self, String> {
        let fail = |reason: &str| format!("pattern binding {:?}: {reason}", raw.name);
        if raw.name.trim().is_empty() {
            return Err(fail("name must not be empty"));
        }
        // Validate standalone syntax first: unmatched groups must not balance
        // against our wrapper and turn full-input anchors into alternatives.
        RegexBuilder::new(&raw.pattern)
            .size_limit(1 << 20)
            .build()
            .map_err(|e| fail(&format!("invalid pattern: {e}")))?;
        let compile = |pattern: &str| RegexBuilder::new(pattern).size_limit(1 << 20).build();
        let regex = compile(&format!(r"\A(?:{})\z", raw.pattern))
            // A trailing verbose-mode comment can consume the closing wrapper.
            // Retry with a newline only on failure; adding one unconditionally
            // would change patterns where verbose mode is disabled.
            .or_else(|_| compile(&format!("\\A(?:{}\n)\\z", raw.pattern)))
            .map_err(|e| fail(&format!("invalid pattern: {e}")))?;

        // Require an explicit, literal HTTP(S) authority. Captures may only
        // occupy path components or query values, never URL syntax or a host.
        let scheme_end = if raw.url.starts_with("https://") {
            8
        } else if raw.url.starts_with("http://") {
            7
        } else {
            return Err(fail("url must start with http:// or https://"));
        };
        if raw
            .url
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
        {
            return Err(fail(
                "url must not contain whitespace, controls or backslashes",
            ));
        }
        let authority_end = raw.url[scheme_end..]
            .find(['/', '?', '#'])
            .map_or(raw.url.len(), |i| i + scheme_end);
        let mut parts = Vec::new();
        let mut cursor = 0;
        while let Some(offset) = raw.url[cursor..].find(['{', '}']) {
            let start = cursor + offset;
            if raw.url.as_bytes()[start] != b'{' {
                return Err(fail("unmatched '}' in url template"));
            }
            let end = raw.url[start + 1..]
                .find('}')
                .map(|i| start + 1 + i)
                .ok_or_else(|| fail("unclosed capture in url template"))?;
            let name = &raw.url[start + 1..end];
            if !regex
                .capture_names()
                .flatten()
                .any(|capture| capture == name)
            {
                return Err(fail(&format!("unknown named capture {{{name}}}")));
            }
            let prefix = &raw.url[..start];
            if start < authority_end || prefix.contains('#') {
                return Err(fail("captures are allowed only in paths and query values"));
            }
            if let Some(query_start) = prefix.find('?') {
                let parameter = prefix[query_start + 1..].rsplit('&').next().unwrap_or("");
                if !parameter.contains('=') {
                    return Err(fail("query captures must appear after a parameter's '='"));
                }
            }
            parts.push(TemplatePart::Literal(raw.url[cursor..start].to_string()));
            parts.push(TemplatePart::Capture(name.to_string()));
            cursor = end + 1;
        }
        parts.push(TemplatePart::Literal(raw.url[cursor..].to_string()));
        let sample: String = parts
            .iter()
            .map(|part| match part {
                TemplatePart::Literal(text) => text.as_str(),
                TemplatePart::Capture(_) => "capture",
            })
            .collect();
        let destination = Url::parse(&sample).map_err(|e| fail(&format!("invalid url: {e}")))?;
        if has_dot_segment(&sample) {
            return Err(fail("url path must not contain dot segments"));
        }
        if destination.host_str().is_none()
            || !destination.username().is_empty()
            || destination.password().is_some()
            || authority_end == scheme_end
        {
            return Err(fail(
                "url requires a fixed host and must not contain credentials",
            ));
        }
        Ok(Self {
            name: raw.name,
            pattern: raw.pattern,
            url: raw.url,
            description: raw.description,
            regex,
            parts,
            destination,
        })
    }
}

impl PatternBinding {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn pattern(&self) -> &str {
        &self.pattern
    }
    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Match the entire, untrimmed input. An unmatched optional capture expands
    /// to an empty value; all supplied capture bytes are percent-encoded once.
    pub fn resolve(&self, input: &str) -> Option<String> {
        let captures = self.regex.captures(input)?;
        let mut output = String::new();
        for part in &self.parts {
            match part {
                TemplatePart::Literal(text) => output.push_str(text),
                TemplatePart::Capture(name) => output.push_str(
                    &utf8_percent_encode(
                        captures.name(name).map_or("", |value| value.as_str()),
                        CAPTURE_ENCODING,
                    )
                    .to_string(),
                ),
            }
        }
        // Defense in depth: substitution must never alter the configured origin.
        // Browsers normalize even percent-encoded '.' and '..' path segments.
        // Such an input must not escape its configured path via normalization.
        if has_dot_segment(&output) {
            return None;
        }
        let mut resolved = Url::parse(&output).ok()?;
        // Only the first '#' may delimit the fragment in an RFC 3986 URI.
        // Url accepts additional literal hashes, while HTTP redirect URIs do not.
        if let Some(fragment) = resolved.fragment() {
            let fragment = utf8_percent_encode(fragment, FRAGMENT_ENCODING).to_string();
            resolved.set_fragment(Some(&fragment));
        }
        (resolved.origin() == self.destination.origin())
            .then(|| utf8_percent_encode(resolved.as_str(), URI_ENCODING).to_string())
    }
}

pub(super) fn deserialize_patterns<'de, D>(deserializer: D) -> Result<Vec<PatternBinding>, D::Error>
where
    D: Deserializer<'de>,
{
    let patterns = Vec::<PatternBinding>::deserialize(deserializer)?;
    let mut names = HashSet::new();
    for pattern in &patterns {
        if !names.insert(pattern.name()) {
            return Err(serde::de::Error::custom(format!(
                "duplicate pattern binding name {:?}",
                pattern.name()
            )));
        }
    }
    Ok(patterns)
}

impl super::BunnylolConfig {
    pub fn resolve_pattern_binding(&self, input: &str) -> Option<String> {
        self.pattern_bindings
            .iter()
            .find_map(|rule| rule.resolve(input))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BunnylolConfig;

    fn rule(pattern: &str, url: &str) -> Result<PatternBinding, String> {
        PatternBindingConfig {
            name: "test".into(),
            pattern: pattern.into(),
            url: url.into(),
            description: None,
        }
        .try_into()
    }

    #[test]
    fn whole_input_including_alternations_and_multiline_flags() {
        let binding = rule(
            r"(?m)(?P<id>ENG-[0-9]+|OCT-[0-9]+)",
            "https://tracker.example/{id}",
        )
        .unwrap();
        assert_eq!(
            binding.resolve("ENG-1234"),
            Some("https://tracker.example/ENG-1234".into())
        );
        for input in [
            "look up ENG-1234",
            " ENG-1234",
            "ENG-1234 ",
            "ENG-1234\n",
            "ENG-1\nOCT-2",
        ] {
            assert!(
                binding.resolve(input).is_none(),
                "{input:?} must not partially match"
            );
        }
    }

    #[test]
    fn capture_values_cannot_inject_url_syntax() {
        let binding = rule(
            r"(?s)(?P<value>.+)",
            "https://example.com/item/{value}?q={value}&fixed=yes",
        )
        .unwrap();
        let input = "//evil.test/a?x=1&y=2#f%20+雪\r\n";
        let encoded = "%2F%2Fevil%2Etest%2Fa%3Fx%3D1%26y%3D2%23f%2520%2B%E9%9B%AA%0D%0A";
        assert_eq!(
            binding.resolve(input),
            Some(format!(
                "https://example.com/item/{encoded}?q={encoded}&fixed=yes"
            ))
        );
        let parsed = Url::parse(&binding.resolve(input).unwrap()).unwrap();
        assert_eq!(parsed.host_str(), Some("example.com"));
        assert_eq!(
            parsed.query_pairs().collect::<Vec<_>>(),
            vec![("q".into(), input.into()), ("fixed".into(), "yes".into())]
        );
    }

    #[test]
    fn invalid_regex_cannot_escape_anchoring_wrapper() {
        assert!(
            rule(
                r"(?P<id>ENG-[0-9]+))|(?:OCT-[0-9]+",
                "https://example.com/{id}",
            )
            .is_err()
        );
    }

    #[test]
    fn verbose_trailing_comments_preserve_full_input_matching() {
        let binding = rule(
            "(?x) (?P<id>ENG-[0-9]+) # ticket",
            "https://example.com/{id}",
        )
        .unwrap();
        assert_eq!(
            binding.resolve("ENG-123"),
            Some("https://example.com/ENG-123".into())
        );
        assert!(binding.resolve("ENG-123 extra").is_none());
        let literal = rule("(?P<id>ENG-[0-9]+) ", "https://example.com/{id}").unwrap();
        assert!(literal.resolve("ENG-123").is_none());
        assert!(literal.resolve("ENG-123 ").is_some());
    }

    #[test]
    fn literal_urls_are_serialized_as_valid_ascii_uris() {
        for (template, expected) in [
            (
                "https://example.com/雪/{id}",
                "https://example.com/%E9%9B%AA/42",
            ),
            ("https://例え.com/{id}", "https://xn--r8jz45g.com/42"),
            (
                "https://example.com/a|b^{id}?q=|^#|^",
                "https://example.com/a%7Cb%5E42?q=%7C%5E#%7C%5E",
            ),
            (
                "https://example.com/{id}#a#b",
                "https://example.com/42#a%23b",
            ),
        ] {
            let binding = rule("(?P<id>[0-9]+)", template).unwrap();
            assert_eq!(binding.resolve("42"), Some(expected.into()));
        }
    }

    #[test]
    fn repeated_and_optional_captures() {
        let binding = rule(
            r"(?P<id>[0-9]+)(?:-(?P<label>[a-z]+))?",
            "http://localhost:9876/{id}?id={id}&label={label}",
        )
        .unwrap();
        assert_eq!(
            binding.resolve("42"),
            Some("http://localhost:9876/42?id=42&label=".into())
        );
        assert_eq!(
            binding.resolve("42-bug"),
            Some("http://localhost:9876/42?id=42&label=bug".into())
        );
    }

    #[test]
    fn dot_segments_cannot_escape_a_path_component() {
        let binding = rule(r"(?P<value>.+)", "https://example.com/items/{value}").unwrap();
        assert!(binding.resolve(".").is_none());
        assert!(binding.resolve("..").is_none());
        assert_eq!(
            binding.resolve("../admin"),
            Some("https://example.com/items/%2E%2E%2Fadmin".into())
        );
        assert!(rule(r"(?P<value>.+)", "https://example.com/../{value}").is_err());
        let combined = rule(r"(?P<value>.+)", "https://example.com/.{value}").unwrap();
        assert!(combined.resolve(".").is_none());
        let query = rule(r"(?P<value>.+)", "https://example.com/?q={value}").unwrap();
        assert_eq!(
            query.resolve(".."),
            Some("https://example.com/?q=%2E%2E".into())
        );
    }

    #[test]
    fn static_url_rules_need_no_captures() {
        let binding = rule("my-dashboard", "https://example.com/dashboard#current").unwrap();
        assert_eq!(
            binding.resolve("my-dashboard"),
            Some("https://example.com/dashboard#current".into())
        );
        assert!(binding.resolve("my-dashboard extra").is_none());
    }

    #[test]
    fn invalid_patterns_and_templates_are_rejected() {
        assert!(rule("[", "https://example.com").is_err());
        assert!(rule(r"(?P<x>.+)", "https://example.com/{unknown}").is_err());
        for url in [
            "javascript:alert(1)",
            "ftp://example.com/{x}",
            "//example.com/{x}",
            "https://{x}/path",
            "https://example.com:{x}/",
            "{x}://example.com/",
            "https://user:pass@example.com/{x}",
            "https://example.com/?{x}=value",
            "https://example.com/#{x}",
            "https://example.com/{x",
            "https://example.com/x}",
            "https://example.com\\evil/{x}",
            "https://example.com/\n{x}",
            "https:///{x}",
            "https://example.com/{0}",
            "https://example.com/{}",
        ] {
            assert!(
                rule(r"(?P<x>.+)", url).is_err(),
                "accepted invalid template {url:?}"
            );
        }
    }

    #[test]
    fn config_validation_and_serialization_preserve_rule_order() {
        let source = r#"
[[pattern_bindings]]
name = 'first'
pattern = '(?P<id>[0-9]+)'
url = 'https://first.example/{id}'
[[pattern_bindings]]
name = 'second'
pattern = '(?P<id>[0-9]+)'
url = 'https://second.example/{id}'
"#;
        let config: BunnylolConfig = toml::from_str(source).unwrap();
        assert_eq!(
            config.resolve_pattern_binding("42"),
            Some("https://first.example/42".into())
        );
        let serialized = config.to_toml_with_comments();
        let reloaded: BunnylolConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(reloaded.pattern_bindings[1].name(), "second");
        assert_eq!(
            reloaded.resolve_pattern_binding("42"),
            config.resolve_pattern_binding("42")
        );
        for invalid in [
            source.replace("name = 'second'", "name = 'first'"),
            source.replace("name = 'first'", "name = ' '"),
            source.replace("name = 'first'", "name = 'first'\nunknown = true"),
        ] {
            assert!(toml::from_str::<BunnylolConfig>(&invalid).is_err());
        }
    }

    #[test]
    fn existing_configuration_has_no_patterns() {
        let config: BunnylolConfig = toml::from_str("default_search = 'ddg'").unwrap();
        assert!(config.pattern_bindings.is_empty());
        assert!(config.resolve_pattern_binding("ENG-1234").is_none());
    }
}
