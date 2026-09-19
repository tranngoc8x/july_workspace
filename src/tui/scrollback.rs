//! Writing finished transcript rows into the terminal's own scrollback.
//!
//! July draws into a fixed band on the last rows of the screen. Everything above that band belongs
//! to the terminal, which means finished rows are written once, as ordinary output, and then scroll
//! away like any other command's output - selectable with the mouse and searchable with the
//! terminal's own find.
//!
//! `Terminal::insert_before` is no help here: it is a no-op for a fixed viewport. Writing the rows
//! directly is also the simpler mechanism, because a line printed on the bottom row makes the
//! terminal scroll by itself, which is exactly the semantics wanted.

use std::io::{self, Write};

use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::style::{Print, PrintStyledContent, ResetColor, StyledContent};
use crossterm::terminal::{Clear, ClearType};
use ratatui::backend::{CrosstermBackend, IntoCrossterm};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Wrap};

use super::support::wrapping::word_wrap_lines;

/// Erases the screen and everything the terminal is holding above it.
///
/// ponytail: `ClearType::Purge` is ANSI-only; on Windows crossterm clears just the visible screen,
/// so a scope switch there leaves the previous scope's rows in scrollback.
pub(super) fn purge<W: Write>(backend: &mut CrosstermBackend<W>) -> io::Result<()> {
    // Order matters: several terminals push what `Clear(All)` erases into the scrollback, so the
    // scrollback has to be dropped after the screen, not before it.
    queue!(
        backend,
        MoveTo(0, 0),
        Clear(ClearType::All),
        Clear(ClearType::Purge),
        MoveTo(0, 0)
    )?;
    backend.flush()
}

/// Writes `lines` above `band`, letting the terminal scroll the older rows into its scrollback.
///
/// Takes every pending row in one call on purpose: each call erases the band before printing, so
/// two calls would have the second one erase what the first just wrote.
///
/// The band is erased first and repainted by the caller's next draw - a row printed while the band
/// is still on screen would be overwritten by it - and the printing ends with one blank row per
/// band row, which is what pushes the new rows clear of the band instead of leaving them under it.
pub(super) fn write_above<W: Write>(
    backend: &mut CrosstermBackend<W>,
    band: Rect,
    lines: Vec<Line<'static>>,
) -> io::Result<()> {
    let width = band.width.max(1);
    let lines: Vec<Line<'static>> = lines.into_iter().map(flatten_style).collect();
    // The terminal would wrap for us, but then we could not tell how many rows we used, and a
    // wrapped row would push the band out of place. Wrapping here keeps one printed line to one
    // screen row.
    let rows = word_wrap_lines(lines.iter(), usize::from(width));
    if rows.is_empty() {
        return Ok(());
    }

    queue!(backend, MoveTo(0, band.y), Clear(ClearType::FromCursorDown))?;
    for row in &rows {
        write_row(backend, row)?;
        queue!(backend, Print("\r\n"))?;
    }
    for _ in 0..band.height {
        queue!(backend, Print("\r\n"))?;
    }
    backend.flush()
}

/// Folds a row's own style into its spans, so that wrapping cannot overwrite them.
///
/// `word_wrap_lines` is the composer's toolkit, where the line style is the outer style and is
/// meant to win: it applies `span.patch_style(line.style)`, which lets the line's colour replace
/// the span's. A transcript row is the other way round - the line only carries the default body
/// colour while each span carries what Markdown decided, so folding the line style in underneath
/// first and then clearing it keeps inline code, quotes and headings their own colour.
fn flatten_style(line: Line<'static>) -> Line<'static> {
    let base = line.style;
    Line {
        spans: line
            .spans
            .into_iter()
            .map(|span| Span {
                style: base.patch(span.style),
                content: span.content,
            })
            .collect(),
        style: Style::default(),
        alignment: line.alignment,
    }
}

fn write_row<W: Write>(backend: &mut CrosstermBackend<W>, row: &Line<'_>) -> io::Result<()> {
    for span in &row.spans {
        let style = row.style.patch(span.style).into_crossterm();
        queue!(
            backend,
            PrintStyledContent(StyledContent::new(style, span.content.as_ref()))
        )?;
    }
    queue!(backend, ResetColor)
}

/// Rows `text` needs at `width`, measured the way the band renders it.
pub(super) fn rendered_rows(text: Text<'static>, width: u16) -> u16 {
    if text.lines.is_empty() {
        return 0;
    }
    Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .line_count(width.max(1))
        .try_into()
        .unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    use super::*;

    #[derive(Clone, Default)]
    struct SharedWriter(Rc<RefCell<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn written(writer: &SharedWriter) -> String {
        String::from_utf8(writer.0.borrow().clone()).unwrap()
    }

    #[test]
    fn rows_are_written_above_the_band_with_their_colour_and_one_newline_each() {
        let writer = SharedWriter::default();
        let mut backend = CrosstermBackend::new(writer.clone());
        let lines = vec![
            Line::from(Span::styled("first", Style::default().fg(Color::Red))),
            Line::from("second"),
        ];

        write_above(&mut backend, Rect::new(0, 8, 40, 4), lines).unwrap();

        let written = written(&writer);
        // Row 9 in one-based ANSI terms is the band's top row: the rows land there and push the
        // older ones up into scrollback.
        assert!(written.contains("\x1b[9;1H"), "{written:?}");
        assert!(written.contains("first"), "{written:?}");
        assert!(written.contains("second"), "{written:?}");
        // Two content rows plus one blank row per band row: the blanks are what push the content
        // clear of the band instead of leaving it underneath.
        assert_eq!(written.matches("\r\n").count(), 2 + 4, "{written:?}");
        assert!(written.contains("\x1b[38;5;1m"), "{written:?}");
    }

    /// End to end: a stored Room message becomes coloured bytes on the terminal.
    ///
    /// The unit tests above feed hand-built rows, so they cannot catch a break between what
    /// `MarkdownStream` produces and what this module expects.
    #[test]
    fn final_and_hydrated_room_messages_reach_the_terminal_with_markdown_colours() {
        use crate::tui::app::{App, AppEvent, Context, HistoryAuthor, HistoryEntry};

        let body = "[agent:pay]\n\nrun `cargo test` then\n\n> check the quote";
        for source in ["final", "hydrated"] {
            let mut app = App::new(Context::root());
            app.reduce(AppEvent::Resize {
                width: 60,
                height: 20,
            });
            match source {
                "final" => {
                    app.reduce(AppEvent::RoomMessage(body.into()));
                }
                "hydrated" => {
                    app.apply_history_for_tests(&[HistoryEntry {
                        author: HistoryAuthor::Agent,
                        body: body.into(),
                    }]);
                }
                _ => unreachable!(),
            };

            let mut rows = Vec::new();
            while let Some(block) = app.take_finished_block() {
                rows.extend(block.lines);
            }
            assert!(
                !rows.is_empty(),
                "{source} message should be ready to write"
            );

            let writer = SharedWriter::default();
            let mut backend = CrosstermBackend::new(writer.clone());
            write_above(&mut backend, Rect::new(0, 16, 60, 4), rows).unwrap();

            let written = written(&writer);
            assert!(
                written.contains("\x1b[38;2;13;205;205mcargo test"),
                "{source} inline code keeps CODE_COLOR:\n{written:?}"
            );
            assert!(
                written.contains("\x1b[38;2;208;215;222mrun "),
                "{source} body text keeps AGENT_COLOR:\n{written:?}"
            );
            assert!(
                written.contains("\x1b[38;5;2m") || written.contains("\x1b[32m"),
                "{source} quote keeps its own colour:\n{written:?}"
            );
        }
    }

    #[test]
    fn room_document_path_has_its_own_colour_in_final_and_hydrated_output() {
        use crate::tui::app::{App, AppEvent, Context, HistoryAuthor, HistoryEntry};

        let body = "Sếp chốt thì em sửa docs/tach-cartpayvoucher-theo-campaign.md mục 4 (bỏ bảng order_transaction, thay bằng ALTER order_detail).";
        for hydrated in [false, true] {
            let mut app = App::new(Context::root());
            if hydrated {
                app.apply_history_for_tests(&[HistoryEntry {
                    author: HistoryAuthor::Agent,
                    body: body.into(),
                }]);
            } else {
                app.reduce(AppEvent::RoomMessage(body.into()));
            }
            let mut rows = Vec::new();
            while let Some(block) = app.take_finished_block() {
                rows.extend(block.lines);
            }
            let writer = SharedWriter::default();
            let mut backend = CrosstermBackend::new(writer.clone());
            write_above(&mut backend, Rect::new(0, 16, 80, 4), rows).unwrap();
            let output = written(&writer);
            assert!(
                output.contains("\x1b[38;2;13;205;205mdocs/tach-cartpayvoucher-theo-campaign.md"),
                "document path needs a distinct foreground (hydrated={hydrated}): {output:?}"
            );
        }
    }

    #[test]
    fn a_span_keeps_its_own_colour_when_the_row_carries_the_body_colour() {
        let writer = SharedWriter::default();
        let mut backend = CrosstermBackend::new(writer.clone());
        // What `markdown::render` produces: the row carries the body colour, each span carries
        // what Markdown decided for it.
        let lines = vec![
            Line::from(vec![
                Span::raw("run "),
                Span::styled("cargo test", Style::default().fg(Color::Rgb(13, 205, 205))),
            ])
            .style(Style::default().fg(Color::Rgb(208, 215, 222))),
        ];

        write_above(&mut backend, Rect::new(0, 8, 40, 2), lines).unwrap();

        let written = written(&writer);
        assert!(
            written.contains("\x1b[38;2;13;205;205mcargo test"),
            "inline code keeps its own colour:\n{written:?}"
        );
        assert!(
            written.contains("\x1b[38;2;208;215;222mrun "),
            "plain text falls back to the body colour:\n{written:?}"
        );
    }

    #[test]
    fn a_long_row_is_wrapped_so_one_printed_line_is_one_screen_row() {
        let writer = SharedWriter::default();
        let mut backend = CrosstermBackend::new(writer.clone());
        let lines = vec![Line::from("alpha beta gamma delta epsilon")];

        write_above(&mut backend, Rect::new(0, 4, 12, 2), lines).unwrap();

        // One printed line per screen row: 30 characters at width 12 cannot be one row, and the
        // band's own two blank rows are on top of that.
        let written = written(&writer);
        assert!(written.matches("\r\n").count() > 2 + 2, "{written:?}");
    }
}
