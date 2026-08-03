use std::{collections::HashSet, ops::Range, path::Path};

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::Deserialize;

use super::{Diagnostic, ParsedNote, ReferenceKind, ReferenceTarget};

#[derive(Debug, Default, Deserialize)]
struct Frontmatter {
    title: Option<String>,
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

    let mut first_h1 = None;
    let mut h1_text = String::new();
    let mut in_h1 = false;
    let mut code_block_start = None;
    let mut code_ranges = Vec::new();
    let mut references = Vec::new();

    let parser = Parser::new_ext(split.body, Options::ENABLE_STRIKETHROUGH).into_offset_iter();
    for (event, range) in parser {
        match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            }) if first_h1.is_none() => {
                in_h1 = true;
                h1_text.clear();
            }
            Event::End(TagEnd::Heading(HeadingLevel::H1)) if in_h1 => {
                let title = h1_text.trim();
                if !title.is_empty() {
                    first_h1 = Some(title.to_owned());
                }
                in_h1 = false;
            }
            Event::Start(Tag::CodeBlock(_)) => code_block_start = Some(range.start),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(start) = code_block_start.take() {
                    code_ranges.push(start..range.end);
                }
            }
            Event::Code(code) => {
                if in_h1 {
                    h1_text.push_str(&code);
                }
                code_ranges.push(range);
            }
            Event::Start(Tag::Link { dest_url, .. }) if code_block_start.is_none() => {
                if let Some(target) = markdown_reference(&dest_url) {
                    references.push(target);
                }
            }
            Event::Text(text) if code_block_start.is_none() && in_h1 => h1_text.push_str(&text),
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

    let fallback_title = relative_path
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("Untitled")
        .to_owned();

    let title = frontmatter
        .as_ref()
        .and_then(|frontmatter| frontmatter.title.as_deref())
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .or(first_h1)
        .unwrap_or(fallback_title);

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

fn scan_tags(text: &str) -> Vec<String> {
    let characters: Vec<(usize, char)> = text.char_indices().collect();
    let mut tags = Vec::new();
    for (position, (byte_index, character)) in characters.iter().enumerate() {
        if *character != '#' {
            continue;
        }
        let previous = position.checked_sub(1).map(|index| characters[index].1);
        if previous
            .is_some_and(|character| character.is_alphanumeric() || "_/-".contains(character))
        {
            continue;
        }
        let start = byte_index + character.len_utf8();
        let mut end = start;
        for (_, character) in characters.iter().skip(position + 1) {
            if character.is_alphanumeric() || matches!(character, '_' | '-' | '/') {
                end += character.len_utf8();
            } else {
                break;
            }
        }
        if end > start {
            let tag = text[start..end].trim_matches('/');
            if !tag.is_empty()
                && tag
                    .chars()
                    .next()
                    .is_some_and(|character| !character.is_ascii_digit())
            {
                tags.push(tag.to_owned());
            }
        }
    }
    tags
}

fn scan_citations(text: &str) -> Vec<String> {
    let characters: Vec<(usize, char)> = text.char_indices().collect();
    let mut citations = Vec::new();
    for (position, (byte_index, character)) in characters.iter().enumerate() {
        if *character != '@' {
            continue;
        }
        let previous = position.checked_sub(1).map(|index| characters[index].1);
        if previous.is_some_and(|character| {
            character.is_alphanumeric() || matches!(character, '_' | '.' | '+' | '-' | '%' | '/')
        }) {
            continue;
        }

        let start = byte_index + character.len_utf8();
        let mut end = start;
        for (_, character) in characters.iter().skip(position + 1) {
            if character.is_alphanumeric()
                || matches!(
                    character,
                    '_' | '-' | ':' | '.' | '/' | '+' | '?' | '$' | '%' | '&' | '~' | '#'
                )
            {
                end += character.len_utf8();
            } else {
                break;
            }
        }

        if end > start {
            let citation = text[start..end]
                .trim_end_matches(['.', ':', '?'])
                .to_owned();
            if !citation.is_empty() {
                citations.push(citation);
            }
        }
    }
    citations
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
    fn title_precedence_is_frontmatter_then_h1_then_filename() {
        assert_eq!(
            parse("file.md", "---\ntitle: Frontmatter\n---\n# Heading").title,
            "Frontmatter"
        );
        assert_eq!(parse("file.md", "# Heading").title, "Heading");
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
        assert_eq!(malformed.title, "Body title");
        assert_eq!(malformed.diagnostics.len(), 1);

        let unclosed = parse("fallback.md", "---\ntitle: Never closes\n# Body title");
        assert_eq!(unclosed.title, "Body title");
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
