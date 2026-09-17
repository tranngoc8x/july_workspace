use std::borrow::Cow;

use pulldown_cmark::{Event, Options as MarkdownOptions, Parser};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use tui_markdown::{Options, StyleSheet, from_str_with_options};

use super::{AGENT_COLOR, CODE_COLOR};

#[derive(Clone, Copy)]
struct JulyStyleSheet;

impl StyleSheet for JulyStyleSheet {
    fn code(&self) -> Style {
        Style::new().fg(CODE_COLOR)
    }

    fn code_block_fence(&self) -> &str {
        ""
    }
}

pub(super) fn render(markdown: &str) -> Text<'static> {
    let text = from_str_with_options(markdown, &Options::new(JulyStyleSheet));
    Text {
        alignment: text.alignment,
        style: text.style,
        lines: text
            .lines
            .into_iter()
            .map(|line| Line {
                alignment: line.alignment,
                style: if line.width() == 0 {
                    line.style
                } else {
                    Style::new().fg(AGENT_COLOR).patch(line.style)
                },
                spans: line
                    .spans
                    .into_iter()
                    .map(|span| Span {
                        content: Cow::Owned(span.content.into_owned()),
                        style: span.style,
                    })
                    .collect(),
            })
            .collect(),
    }
}

#[derive(Default)]
pub(super) struct MarkdownStream {
    completed: Vec<Text<'static>>,
    tail: String,
}

impl MarkdownStream {
    pub(super) fn push(&mut self, delta: &str) {
        self.tail.push_str(delta);
        let ranges = top_level_blocks(&self.tail);
        let mut cursor = 0;
        for blocks in ranges.windows(2) {
            let separator = &self.tail[blocks[0].1..blocks[1].0];
            if !separator.contains('\n') {
                continue;
            }
            self.completed.push(render(&self.tail[cursor..blocks[0].1]));
            cursor = blocks[0].1;
            let block = self.completed.last_mut().unwrap();
            let trailing = block
                .lines
                .iter()
                .rev()
                .take_while(|line| line.width() == 0)
                .count();
            block.lines.extend(std::iter::repeat_n(
                Line::default(),
                1usize.saturating_sub(trailing),
            ));
            if separator.trim().is_empty() {
                cursor = blocks[1].0;
            }
        }
        if cursor > 0 {
            self.tail.drain(..cursor);
        }
    }

    pub(super) fn finish(&mut self) {
        if !self.tail.is_empty() {
            self.completed.push(render(&self.tail));
            self.tail.clear();
        }
    }

    pub(super) fn push_plain(&mut self, text: String, color: Color) {
        let style = Style::new().fg(color);
        let lines = Text::from(text)
            .lines
            .into_iter()
            .map(|line| line.patch_style(style))
            .collect::<Vec<_>>();
        self.completed.push(Text::from(lines));
    }

    pub(super) fn tail(&self) -> &str {
        &self.tail
    }

    /// Hands out the oldest finished block, which the caller writes to the terminal's scrollback.
    ///
    /// Popping rather than indexing is what makes a double-write unrepresentable: what is left in
    /// `completed` is exactly what has not been written yet, so rebuilding the stream on a scope
    /// switch needs no bookkeeping of its own.
    pub(super) fn pop_completed(&mut self) -> Option<Text<'static>> {
        (!self.completed.is_empty()).then(|| self.completed.remove(0))
    }

    pub(super) fn text(&self) -> Text<'static> {
        let mut text = Text::default();
        for block in &self.completed {
            text.lines.extend(block.lines.clone());
        }
        if !self.tail.is_empty() {
            text.lines.extend(render(&self.tail).lines);
        }
        text
    }

    #[cfg(test)]
    pub(super) fn completed_len(&self) -> usize {
        self.completed.len()
    }

    #[cfg(test)]
    pub(super) fn completed(&self) -> Text<'static> {
        let mut text = Text::default();
        for block in &self.completed {
            text.lines.extend(block.lines.clone());
        }
        text
    }
}

fn top_level_blocks(markdown: &str) -> Vec<(usize, usize)> {
    let mut depth = 0usize;
    let mut start = None;
    let mut blocks = Vec::new();

    for (event, range) in Parser::new_ext(markdown, markdown_options()).into_offset_iter() {
        match event {
            Event::Start(_) => {
                if depth == 0 {
                    start = Some(range.start);
                }
                depth += 1;
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 0
                    && let Some(start) = start.take()
                {
                    blocks.push((start, range.end));
                }
            }
            _ => {}
        }
    }
    blocks
}

fn markdown_options() -> MarkdownOptions {
    // Keep boundary parsing identical to the pinned tui-markdown 0.3.9 renderer.
    let mut options = MarkdownOptions::empty();
    options.insert(MarkdownOptions::ENABLE_STRIKETHROUGH);
    options.insert(MarkdownOptions::ENABLE_TASKLISTS);
    options.insert(MarkdownOptions::ENABLE_HEADING_ATTRIBUTES);
    options.insert(MarkdownOptions::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    options.insert(MarkdownOptions::ENABLE_SUPERSCRIPT);
    options.insert(MarkdownOptions::ENABLE_SUBSCRIPT);
    options.insert(MarkdownOptions::ENABLE_MATH);
    options.insert(MarkdownOptions::ENABLE_FOOTNOTES);
    options.insert(MarkdownOptions::ENABLE_DEFINITION_LIST);
    options.insert(MarkdownOptions::ENABLE_GFM);
    options.insert(MarkdownOptions::ENABLE_TABLES);
    options
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::MarkdownStream;

    fn completed(chunks: impl IntoIterator<Item = String>) -> ratatui::text::Text<'static> {
        let mut stream = MarkdownStream::default();
        for chunk in chunks {
            stream.push(&chunk);
        }
        stream.finish();
        stream.text()
    }

    #[test]
    fn agent_body_and_inline_code_use_the_palette_without_hiding_markdown_styles() {
        let rendered = super::render("result `code`\n\n> quote");
        let foreground = |content: &str| {
            rendered.lines.iter().find_map(|line| {
                line.spans.iter().find_map(|span| {
                    (span.content == content)
                        .then(|| rendered.style.patch(line.style).patch(span.style).fg)
                        .flatten()
                })
            })
        };

        assert_eq!(foreground("result "), Some(Color::Rgb(208, 215, 222)));
        assert_eq!(foreground("code"), Some(Color::Rgb(13, 205, 205)));
        assert_eq!(foreground("quote"), Some(Color::Green));
    }

    #[test]
    fn adversarial_chunks_match_one_shot_markdown_rendering() {
        let fixtures = [
            "A **bold** paragraph with [link](https://example.com).\n\nNext paragraph.",
            "> quoted *text*\n\n- first\n- second\n\nAfter list.",
            "```rust\nfn main() {}\n```\n\nAfter code.",
            "| Name | Status |\n|:-----|-------:|\n| API | Ready |\n\nAfter table.",
            "---\ntitle: Demo\n---\n\nAfter metadata.",
        ];

        for markdown in fixtures {
            let expected = super::render(markdown);
            for split in markdown
                .char_indices()
                .map(|(index, _)| index)
                .filter(|index| *index > 0)
            {
                assert_eq!(
                    completed([markdown[..split].to_owned(), markdown[split..].to_owned()]),
                    expected,
                    "split at byte {split} in {markdown:?}"
                );
            }
            assert_eq!(
                completed(markdown.chars().map(|character| character.to_string())),
                expected,
                "character chunks in {markdown:?}"
            );
        }
    }

    #[test]
    fn frozen_reference_stays_literal_when_a_definition_arrives_later() {
        let mut stream = MarkdownStream::default();
        stream.push("[label][later]\n\nsecond paragraph");
        let frozen = stream.completed();

        stream.push("\n\n[later]: https://example.com");

        assert_eq!(frozen.to_string(), "[label][later]\n");
        assert_eq!(stream.completed(), frozen);
    }

    #[test]
    fn hidden_reference_definition_preserves_inter_block_spacing() {
        let mut stream = MarkdownStream::default();

        stream.push("First\n\n[id]: /url\n\nSecond");

        assert_eq!(stream.text().to_string(), "First\n\nSecond");
    }
}
