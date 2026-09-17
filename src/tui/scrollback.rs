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
use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, Wrap};

use super::support::wrapping::word_wrap_lines;

/// Erases the screen and everything the terminal is holding above it.
///
/// ponytail: `ClearType::Purge` is ANSI-only; on Windows crossterm clears just the visible screen,
/// so a scope switch there leaves the previous scope's rows in scrollback.
pub(super) fn purge<W: Write>(backend: &mut CrosstermBackend<W>) -> io::Result<()> {
    queue!(
        backend,
        MoveTo(0, 0),
        Clear(ClearType::Purge),
        Clear(ClearType::All)
    )?;
    backend.flush()
}

/// Writes `text` above `band`, letting the terminal scroll the older rows into its scrollback.
///
/// The band is erased first and repainted by the caller's next draw: a row printed while the band
/// is still on screen would be overwritten by it.
pub(super) fn write_above<W: Write>(
    backend: &mut CrosstermBackend<W>,
    band: Rect,
    text: Text<'static>,
) -> io::Result<()> {
    let width = band.width.max(1);
    // The terminal wraps for us, but then we could not tell how many rows we used, and a wrapped
    // row would break the band's position. Wrapping here keeps one printed line to one screen row.
    let rows = word_wrap_lines(text.lines.iter(), usize::from(width));
    if rows.is_empty() {
        return Ok(());
    }

    queue!(backend, MoveTo(0, band.y), Clear(ClearType::FromCursorDown))?;
    for row in &rows {
        write_row(backend, row)?;
        queue!(backend, Print("\r\n"))?;
    }
    backend.flush()
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
        let text = Text::from(vec![
            Line::from(Span::styled("first", Style::default().fg(Color::Red))),
            Line::from("second"),
        ]);

        write_above(&mut backend, Rect::new(0, 8, 40, 4), text).unwrap();

        let written = written(&writer);
        // Row 9 in one-based ANSI terms is the band's top row: the rows land there and push the
        // older ones up into scrollback.
        assert!(written.contains("\x1b[9;1H"), "{written:?}");
        assert!(written.contains("first"), "{written:?}");
        assert!(written.contains("second"), "{written:?}");
        assert_eq!(written.matches("\r\n").count(), 2, "{written:?}");
        assert!(written.contains("\x1b[38;5;1m"), "{written:?}");
    }

    #[test]
    fn a_long_row_is_wrapped_so_one_printed_line_is_one_screen_row() {
        let writer = SharedWriter::default();
        let mut backend = CrosstermBackend::new(writer.clone());
        let text = Text::from(Line::from("alpha beta gamma delta epsilon"));

        write_above(&mut backend, Rect::new(0, 4, 12, 2), text).unwrap();

        let written = written(&writer);
        assert!(written.matches("\r\n").count() > 1, "{written:?}");
    }
}
