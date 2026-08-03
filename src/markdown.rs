use std::{collections::HashMap, sync::Arc};

use eframe::egui::{
    self, Color32, FontFamily, FontId, Galley, Stroke, TextFormat,
    text::{LayoutJob, TextWrapping},
};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::vault::{NoteId, NoteRecord};

const CARD_EXCERPT_CHARS: usize = 4_096;
pub const CARD_BODY_FONT_WORLD: f32 = 3.7;
pub const CARD_BODY_WIDTH_WORLD: f32 = 202.0;
const CARD_SCALE_STEPS: f32 = 16.0;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum CacheLayout {
    CardScale { scale: u16, full_body: bool },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CacheKey {
    note_id: NoteId,
    layout: CacheLayout,
}

#[derive(Clone, Copy, Debug)]
struct LayoutSpec {
    cache_layout: CacheLayout,
    wrap_width: f32,
    base_font_size: f32,
    excerpt_only: bool,
}

#[derive(Debug, Default)]
pub struct MarkdownCache {
    galleys: HashMap<CacheKey, Arc<Galley>>,
    pixels_per_point: Option<u32>,
}

impl MarkdownCache {
    pub fn clear(&mut self) {
        self.galleys.clear();
        self.pixels_per_point = None;
    }

    pub fn card_galley(
        &mut self,
        painter: &egui::Painter,
        note: &NoteRecord,
        scale: f32,
        full_body: bool,
        pixels_per_point: f32,
    ) -> Arc<Galley> {
        let scale_bucket = card_scale_bucket(scale);
        let layout_scale = f32::from(scale_bucket) / CARD_SCALE_STEPS;
        self.galley(
            painter,
            note,
            LayoutSpec {
                cache_layout: CacheLayout::CardScale {
                    scale: scale_bucket,
                    full_body,
                },
                wrap_width: CARD_BODY_WIDTH_WORLD * layout_scale,
                base_font_size: CARD_BODY_FONT_WORLD * layout_scale,
                excerpt_only: !full_body,
            },
            pixels_per_point,
        )
    }

    fn galley(
        &mut self,
        painter: &egui::Painter,
        note: &NoteRecord,
        spec: LayoutSpec,
        pixels_per_point: f32,
    ) -> Arc<Galley> {
        let pixels_key = pixels_per_point.to_bits();
        if self.pixels_per_point != Some(pixels_key) {
            self.galleys.clear();
            self.pixels_per_point = Some(pixels_key);
        }

        let key = CacheKey {
            note_id: note.id.clone(),
            layout: spec.cache_layout,
        };
        if let Some(galley) = self.galleys.get(&key) {
            return Arc::clone(galley);
        }

        let body = without_duplicate_title(&note.markdown_body, &note.title);
        let markdown = if spec.excerpt_only {
            excerpt(body, CARD_EXCERPT_CHARS)
        } else {
            body
        };
        let galley =
            painter.layout_job(markdown_job(markdown, spec.wrap_width, spec.base_font_size));
        self.galleys.insert(key, Arc::clone(&galley));
        galley
    }
}

fn card_scale_bucket(scale: f32) -> u16 {
    (scale.max(1.0 / CARD_SCALE_STEPS) * CARD_SCALE_STEPS)
        .round()
        .clamp(1.0, f32::from(u16::MAX)) as u16
}

fn excerpt(source: &str, max_chars: usize) -> &str {
    source
        .char_indices()
        .nth(max_chars)
        .map_or(source, |(end, _)| &source[..end])
}

fn without_duplicate_title<'a>(markdown: &'a str, title: &str) -> &'a str {
    let trimmed_start = markdown.trim_start_matches([' ', '\t', '\r', '\n']);
    let Some(first_line_end) = trimmed_start.find('\n') else {
        return if trimmed_start
            .strip_prefix("# ")
            .is_some_and(|heading| heading.trim() == title.trim())
        {
            ""
        } else {
            markdown
        };
    };
    let first_line = trimmed_start[..first_line_end].trim_end_matches('\r');
    if first_line
        .strip_prefix("# ")
        .is_some_and(|heading| heading.trim() == title.trim())
    {
        trimmed_start[first_line_end + 1..].trim_start_matches(['\r', '\n'])
    } else {
        markdown
    }
}

fn markdown_job(markdown: &str, wrap_width: f32, base_size: f32) -> LayoutJob {
    let mut job = LayoutJob {
        wrap: TextWrapping {
            max_width: wrap_width,
            ..TextWrapping::default()
        },
        ..LayoutJob::default()
    };
    let mut style = MarkdownStyle::default();
    let options = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;

    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Start(tag) => style.start(&tag, &mut job, base_size),
            Event::End(tag) => style.end(tag, &mut job, base_size),
            Event::Text(text) => append(&mut job, &text, style.format(base_size)),
            Event::Code(code) => {
                let mut format = style.format(base_size);
                format.font_id.family = FontFamily::Monospace;
                format.background = Color32::from_rgb(235, 235, 229);
                append(&mut job, &code, format);
            }
            Event::SoftBreak => append(&mut job, " ", style.format(base_size)),
            Event::HardBreak => append(&mut job, "\n", style.format(base_size)),
            Event::Rule => append(
                &mut job,
                "\n────────────────────────\n",
                style.format(base_size),
            ),
            Event::TaskListMarker(checked) => append(
                &mut job,
                if checked { "☑ " } else { "☐ " },
                style.format(base_size),
            ),
            Event::FootnoteReference(reference) => append(
                &mut job,
                &format!("[{reference}]"),
                style.format(base_size * 0.85),
            ),
            Event::InlineHtml(html) | Event::Html(html) => {
                append(&mut job, &html, style.format(base_size * 0.9));
            }
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                let mut format = style.format(base_size);
                format.font_id.family = FontFamily::Monospace;
                append(&mut job, &math, format);
            }
        }
    }

    job
}

#[derive(Debug, Default)]
struct MarkdownStyle {
    heading: Option<HeadingLevel>,
    emphasis_depth: usize,
    strong_depth: usize,
    strike_depth: usize,
    code_block_depth: usize,
    quote_depth: usize,
    link_depth: usize,
}

impl MarkdownStyle {
    fn start(&mut self, tag: &Tag<'_>, job: &mut LayoutJob, base_size: f32) {
        match tag {
            Tag::Heading { level, .. } => self.heading = Some(*level),
            Tag::Emphasis => self.emphasis_depth += 1,
            Tag::Strong => self.strong_depth += 1,
            Tag::Strikethrough => self.strike_depth += 1,
            Tag::CodeBlock(_) => {
                self.code_block_depth += 1;
                ensure_block_break(job, self.format(base_size));
            }
            Tag::BlockQuote(_) => {
                self.quote_depth += 1;
                ensure_block_break(job, self.format(base_size));
                append(job, "│ ", self.format(base_size));
            }
            Tag::Item => append(job, "• ", self.format(base_size)),
            Tag::FootnoteDefinition(label) => {
                append(job, &format!("[{label}] "), self.format(base_size * 0.9))
            }
            Tag::Link { .. } => self.link_depth += 1,
            Tag::TableCell if !job.text.ends_with(['\n', ' ']) => {
                append(job, "  ·  ", self.format(base_size));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd, job: &mut LayoutJob, base_size: f32) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock => {
                append(job, "\n\n", self.format(base_size));
            }
            TagEnd::Item | TagEnd::TableRow => append(job, "\n", self.format(base_size)),
            TagEnd::BlockQuote(_) => {
                self.quote_depth = self.quote_depth.saturating_sub(1);
                append(job, "\n", self.format(base_size));
            }
            TagEnd::Emphasis => self.emphasis_depth = self.emphasis_depth.saturating_sub(1),
            TagEnd::Strong => self.strong_depth = self.strong_depth.saturating_sub(1),
            TagEnd::Strikethrough => self.strike_depth = self.strike_depth.saturating_sub(1),
            TagEnd::Link => self.link_depth = self.link_depth.saturating_sub(1),
            _ => {}
        }
        if matches!(tag, TagEnd::Heading(_)) {
            self.heading = None;
        }
        if matches!(tag, TagEnd::CodeBlock) {
            self.code_block_depth = self.code_block_depth.saturating_sub(1);
        }
    }

    fn format(&self, base_size: f32) -> TextFormat {
        let heading_multiplier = match self.heading {
            Some(HeadingLevel::H1) => 1.65,
            Some(HeadingLevel::H2) => 1.4,
            Some(HeadingLevel::H3) => 1.2,
            Some(_) => 1.05,
            None => 1.0,
        };
        let mut format = TextFormat {
            font_id: FontId::new(
                base_size * heading_multiplier,
                if self.code_block_depth > 0 {
                    FontFamily::Monospace
                } else {
                    FontFamily::Proportional
                },
            ),
            color: if self.quote_depth > 0 {
                Color32::from_rgb(84, 94, 85)
            } else {
                Color32::from_rgb(45, 49, 43)
            },
            italics: self.emphasis_depth > 0,
            ..TextFormat::default()
        };
        if self.strong_depth > 0 || self.heading.is_some() {
            format.extra_letter_spacing = 0.2;
        }
        if self.strike_depth > 0 {
            format.strikethrough = Stroke::new(1.0, format.color);
        }
        if self.link_depth > 0 {
            format.color = Color32::from_rgb(58, 103, 82);
            format.underline = Stroke::new(1.0, format.color);
        }
        if self.code_block_depth > 0 {
            format.background = Color32::from_rgb(238, 238, 232);
        }
        format
    }
}

fn append(job: &mut LayoutJob, text: &str, format: TextFormat) {
    job.append(text, 0.0, format);
}

fn ensure_block_break(job: &mut LayoutJob, format: TextFormat) {
    if !job.text.is_empty() && !job.text.ends_with('\n') {
        append(job, "\n", format);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CARD_BODY_FONT_WORLD, CARD_BODY_WIDTH_WORLD, CARD_SCALE_STEPS, card_scale_bucket, excerpt,
        markdown_job, without_duplicate_title,
    };

    #[test]
    fn card_excerpt_stops_on_a_character_boundary() {
        assert_eq!(excerpt("aé日z", 3), "aé日");
        assert_eq!(excerpt("short", 20), "short");
    }

    #[test]
    fn cache_layouts_are_quantized() {
        assert_eq!(card_scale_bucket(1.0), CARD_SCALE_STEPS as u16);
        assert_eq!(card_scale_bucket(1.03), CARD_SCALE_STEPS as u16);
    }

    #[test]
    fn zoom_preserves_the_markdown_width_to_font_ratio() {
        let at_one = CARD_BODY_WIDTH_WORLD / CARD_BODY_FONT_WORLD;
        let at_four = (CARD_BODY_WIDTH_WORLD * 4.0) / (CARD_BODY_FONT_WORLD * 4.0);
        assert!((at_one - at_four).abs() < f32::EPSILON);
    }

    #[test]
    fn markdown_layout_preserves_content_and_removes_markup_tokens() {
        let job = markdown_job("# Heading\n\n**Strong** and `code`.", 400.0, 15.0);
        assert!(job.text.contains("Heading"));
        assert!(job.text.contains("Strong and code."));
        assert!(!job.text.contains("**"));
    }

    #[test]
    fn duplicate_leading_h1_is_not_rendered_below_the_note_title() {
        assert_eq!(
            without_duplicate_title("# Note title\n\nBody", "Note title"),
            "Body"
        );
        assert_eq!(
            without_duplicate_title("# Different\n\nBody", "Note title"),
            "# Different\n\nBody"
        );
    }
}
