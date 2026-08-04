use std::{collections::HashSet, ops::Range, path::Path};

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde::Deserialize;

use super::{Diagnostic, ParsedNote, ReferenceKind, ReferenceTarget};

#[derive(Debug, Default, Deserialize)]
struct Frontmatter {
    #[serde(default, deserialize_with = "deserialize_one_or_many")]
    tags: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_one_or_many")]
    aliases: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

fn deserialize_one_or_many<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<OneOrMany>::deserialize(deserializer)? {
        Some(OneOrMany::One(value)) => vec![value],
        Some(OneOrMany::Many(values)) => values,
        None => Vec::new(),
    })
}

struct FrontmatterSplit<'a> {
    yaml: Option<&'a str>,
    body: &'a str,
    unclosed: bool,
}

pub fn parse_note(root: &Path, path: &Path, source: &str) -> ParsedNote {
    let relative_path = path.strip_prefix(root).unwrap_or(path).to_path_buf();
    let split = split_frontmatter(source);
    let mut diagnostics = Vec::new();

    if split.unclosed {
        diagnostics.push(Diagnostic::warning(
            Some(relative_path.clone()),
            "Frontmatter starts with `---` but has no closing delimiter",
        ));
    }

    let frontmatter =
        split
            .yaml
            .and_then(|yaml| match serde_yaml_ng::from_str::<Frontmatter>(yaml) {
                Ok(frontmatter) => Some(frontmatter),
                Err(error) => {
                    diagnostics.push(Diagnostic::warning(
                        Some(relative_path.clone()),
                        format!("Could not parse YAML frontmatter: {error}"),
                    ));
                    None
                }
            });

    let mut code_block_start = None;
    let mut code_ranges = Vec::new();
    let mut references = Vec::new();

    let parser = Parser::new_ext(split.body, Options::ENABLE_STRIKETHROUGH).into_offset_iter();
    for (event, range) in parser {
        match event {
            Event::Start(Tag::CodeBlock(_)) => code_block_start = Some(range.start),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = code_block_start.take() {
                    code_ranges.push(start..range.end);
                }
            }
            Event::Code(_) => code_ranges.push(range),
            Event::Start(Tag::Link { dest_url, .. }) if code_block_start.is_none() => {
                if let Some(target) = markdown_reference(&dest_url) {
                    references.push(target);
                }
            }
            _ => {}
        }
    }
    if let Some(start) = code_block_start {
        code_ranges.push(start..split.body.len());
    }

    let mut inline_tags = Vec::new();
    let mut citations = Vec::new();
    for segment in non_code_segments(split.body, code_ranges) {
        inline_tags.extend(scan_tags(segment));
        references.extend(scan_wikilinks(segment));
        citations.extend(scan_citations(segment));
    }

    let title = relative_path
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("Untitled")
        .to_owned();

    let aliases = deduplicate_strings(
        frontmatter
            .as_ref()
            .map(|frontmatter| frontmatter.aliases.clone())
            .unwrap_or_default(),
        false,
    );

    let frontmatter_tags = frontmatter
        .as_ref()
        .map(|frontmatter| frontmatter.tags.clone())
        .unwrap_or_default();
    let tags = deduplicate_strings(
        frontmatter_tags.into_iter().chain(inline_tags).collect(),
        true,
    );

    ParsedNote {
        relative_path,
        title,
        markdown_body: split.body.to_owned(),
        aliases,
        tags,
        unresolved_references: deduplicate_references(references),
        citations: deduplicate_strings(citations, false),
        diagnostics,
    }
}

fn non_code_segments(source: &str, mut excluded: Vec<Range<usize>>) -> Vec<&str> {
    excluded.sort_by_key(|range| range.start);
    let mut segments = Vec::new();
    let mut cursor = 0;
    for range in excluded {
        let start = range.start.min(source.len()).max(cursor);
        let end = range.end.min(source.len()).max(start);
        if cursor < start {
            segments.push(&source[cursor..start]);
        }
        cursor = cursor.max(end);
    }
    if cursor < source.len() {
        segments.push(&source[cursor..]);
    }
    segments
}

fn split_frontmatter(source: &str) -> FrontmatterSplit<'_> {
    let first_newline = source.find('\n').unwrap_or(source.len());
    let first_line = source[..first_newline].trim_end_matches('\r');
    if first_line != "---" {
        return FrontmatterSplit {
            yaml: None,
            body: source,
            unclosed: false,
        };
    }

    let yaml_start = (first_newline < source.len()).then_some(first_newline + 1);
    let Some(yaml_start) = yaml_start else {
        return FrontmatterSplit {
            yaml: None,
            body: source,
            unclosed: true,
        };
    };

    let mut cursor = yaml_start;
    while cursor <= source.len() {
        let relative_end = source[cursor..].find('\n');
        let line_end = relative_end.map_or(source.len(), |offset| cursor + offset);
        let line = source[cursor..line_end].trim_end_matches('\r');
        if line == "---" || line == "..." {
            let body_start = if line_end < source.len() {
                line_end + 1
            } else {
                line_end
            };
            return FrontmatterSplit {
                yaml: Some(&source[yaml_start..cursor]),
                body: &source[body_start..],
                unclosed: false,
            };
        }
        if line_end == source.len() {
            break;
        }
        cursor = line_end + 1;
    }

    FrontmatterSplit {
        yaml: None,
        body: source,
        unclosed: true,
    }
}

fn markdown_reference(destination: &str) -> Option<ReferenceTarget> {
    let destination = destination.trim().trim_matches(['<', '>']);
    if destination.is_empty()
        || destination.starts_with('#')
        || destination.starts_with("//")
        || destination.contains("://")
        || destination.starts_with("mailto:")
    {
        return None;
    }

    let path = destination.split(['#', '?']).next()?.trim();
    path.to_ascii_lowercase()
        .ends_with(".md")
        .then(|| ReferenceTarget {
            raw: path.to_owned(),
            kind: ReferenceKind::MarkdownLink,
        })
}

fn scan_wikilinks(text: &str) -> Vec<ReferenceTarget> {
    let mut references = Vec::new();
    let mut cursor = 0;
    while let Some(start_offset) = text[cursor..].find("[[") {
        let content_start = cursor + start_offset + 2;
        let Some(end_offset) = text[content_start..].find("]]") else {
            break;
        };
        let content_end = content_start + end_offset;
        let raw = text[content_start..content_end]
            .split('|')
            .next()
            .unwrap_or_default()
            .split('#')
            .next()
            .unwrap_or_default()
            .trim();
        if !raw.is_empty() {
            references.push(ReferenceTarget {
                raw: raw.to_owned(),
                kind: ReferenceKind::WikiLink,
            });
        }
        cursor = content_end + 2;
    }
    references
}

fn scan_sigil_tokens(
    text: &str,
    sigil: char,
    attaches_to: fn(char) -> bool,
    is_body: fn(char) -> bool,
) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut previous = None;
    for (index, character) in text.char_indices() {
        if character == sigil && !previous.is_some_and(attaches_to) {
            let start = index + character.len_utf8();
            let end = text[start..]
                .find(|character: char| !is_body(character))
                .map_or(text.len(), |offset| start + offset);
            if end > start {
                tokens.push(&text[start..end]);
            }
        }
        previous = Some(character);
    }
    tokens
}

fn scan_tags(text: &str) -> Vec<String> {
    scan_sigil_tokens(text, '#', is_tag_character, is_tag_character)
        .into_iter()
        .filter_map(|token| {
            let tag = token.trim_matches('/');
            let starts_with_digit = tag
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_digit());
            (!tag.is_empty() && !starts_with_digit).then(|| tag.to_owned())
        })
        .collect()
}

fn is_tag_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '-' | '/')
}

fn scan_citations(text: &str) -> Vec<String> {
    scan_sigil_tokens(text, '@', attaches_to_citation, is_citation_character)
        .into_iter()
        .filter_map(|token| {
            let citation = token.trim_end_matches(['.', ':', '?']);
            (!citation.is_empty()).then(|| citation.to_owned())
        })
        .collect()
}

fn attaches_to_citation(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '.' | '+' | '-' | '%' | '/')
}

fn is_citation_character(character: char) -> bool {
    character.is_alphanumeric()
        || matches!(
            character,
            '_' | '-' | ':' | '.' | '/' | '+' | '?' | '$' | '%' | '&' | '~' | '#'
        )
}

fn deduplicate_strings(values: Vec<String>, strip_hash: bool) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter_map(|value| {
            let value = value.trim();
            let value = if strip_hash {
                value.strip_prefix('#').unwrap_or(value)
            } else {
                value
            };
            if value.is_empty() || !seen.insert(value.to_lowercase()) {
                None
            } else {
                Some(value.to_owned())
            }
        })
        .collect()
}

fn deduplicate_references(references: Vec<ReferenceTarget>) -> Vec<ReferenceTarget> {
    let mut seen = HashSet::new();
    references
        .into_iter()
        .filter(|reference| seen.insert(reference.raw.to_lowercase()))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::parse_note;

    fn parse(path: &str, source: &str) -> super::ParsedNote {
        parse_note(
            Path::new("/vault"),
            Path::new("/vault").join(path).as_path(),
            source,
        )
    }

    #[test]
    fn title_is_always_the_filename_without_the_markdown_extension() {
        assert_eq!(
            parse("file.md", "---\ntitle: Frontmatter\n---\n# Heading").title,
            "file"
        );
        assert_eq!(parse("Nested Note.MD", "# Heading").title, "Nested Note");
        assert_eq!(parse("file.md", "Body").title, "file");
    }

    #[test]
    fn frontmatter_accepts_scalar_and_list_tags_and_aliases() {
        let scalar = parse(
            "file.md",
            "---\ntags: '#Methods'\naliases: Alternate\n---\nBody",
        );
        assert_eq!(scalar.tags, ["Methods"]);
        assert_eq!(scalar.aliases, ["Alternate"]);

        let list = parse(
            "file.md",
            "---\ntags: [Methods, evidence]\naliases:\n  - Alternate\n  - Other\n---\nBody",
        );
        assert_eq!(list.tags, ["Methods", "evidence"]);
        assert_eq!(list.aliases, ["Alternate", "Other"]);
    }

    #[test]
    fn inline_tags_are_nested_deduplicated_and_exclude_code() {
        let note = parse(
            "file.md",
            "# Heading\n#Methods #methods #methods/causal `#inline`\n```\n#fenced\n```",
        );
        assert_eq!(note.tags, ["Methods", "methods/causal"]);
    }

    #[test]
    fn wikilinks_strip_aliases_and_headings_and_include_embeds() {
        let note = parse(
            "file.md",
            "[[Note#Section|Display]] ![[folder/Other]] `[[Ignored]]`",
        );
        let targets: Vec<_> = note
            .unresolved_references
            .iter()
            .map(|reference| reference.raw.as_str())
            .collect();
        assert_eq!(targets, ["Note", "folder/Other"]);
    }

    #[test]
    fn local_markdown_links_are_references_but_external_links_are_not() {
        let note = parse(
            "file.md",
            "[Local](folder/note.md#part) [Web](https://example.com/a.md) [PDF](a.pdf)",
        );
        assert_eq!(note.unresolved_references.len(), 1);
        assert_eq!(note.unresolved_references[0].raw, "folder/note.md");
    }

    #[test]
    fn citations_include_bracketed_multiple_and_narrative_forms() {
        let note = parse(
            "file.md",
            "See [@smith2020; @jones-2021] and @narrative. Ignore me@example.com, `@inline`, and:\n```\n@fenced\n```",
        );
        assert_eq!(note.citations, ["smith2020", "jones-2021", "narrative"]);
    }

    #[test]
    fn malformed_and_unclosed_frontmatter_fall_back_without_panicking() {
        let malformed = parse("fallback.md", "---\ntitle: [broken\n---\n# Body title");
        assert_eq!(malformed.title, "fallback");
        assert_eq!(malformed.diagnostics.len(), 1);

        let unclosed = parse("fallback.md", "---\ntitle: Never closes\n# Body title");
        assert_eq!(unclosed.title, "fallback");
        assert_eq!(unclosed.diagnostics.len(), 1);
    }

    #[test]
    fn retained_markdown_body_excludes_valid_frontmatter() {
        let note = parse(
            "file.md",
            "---\ntitle: Frontmatter\ntags: test\n---\n# Heading\n\nBody text",
        );
        assert_eq!(note.markdown_body, "# Heading\n\nBody text");
        assert!(!note.markdown_body.contains("title: Frontmatter"));
    }
}
