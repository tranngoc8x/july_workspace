use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::{Block, Paragraph, Wrap};

use super::app::App;

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < 12 || area.height < 6 {
        frame.render_widget(Paragraph::new("July"), area);
        return;
    }

    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(area);
    let status = app.status().map_or_else(
        || app.context().label().to_owned(),
        |status| format!("{} — {status}", app.context().label()),
    );
    frame.render_widget(Paragraph::new(status), areas[0]);

    frame.render_widget(
        Paragraph::new(app.transcript_text())
            .wrap(Wrap { trim: false })
            .scroll((app.transcript_scroll(), 0)),
        areas[1],
    );

    let input = Block::bordered().title("input");
    let input_area = input.inner(areas[2]);
    frame.render_widget(input, areas[2]);
    frame.render_widget(app.input_widget(), input_area);
    frame.render_widget(
        Paragraph::new("Enter send · Alt+Enter newline · Esc exit"),
        areas[3],
    );
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::application::ChatEvent;
    use crate::tui::app::{App, AppEvent, Context};

    use super::render;

    const TALL_MARKDOWN: &str = "0  \n1  \n2  \n3  \n4  \n5  \n6  \n7  \n8  \n9";

    #[test]
    fn tiny_terminal_render_is_bounded_and_keeps_the_july_label() {
        let app = App::new(Context::root());
        let mut terminal = Terminal::new(TestBackend::new(8, 2)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((0, 0)).unwrap().symbol(), "J");
    }

    #[test]
    fn tall_transcript_follows_the_actual_tail() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 20,
            height: 10,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(TALL_MARKDOWN.into())));
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));
        let mut terminal = Terminal::new(TestBackend::new(20, 10)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        assert_eq!(
            terminal.backend().buffer().cell((0, 1)).unwrap().symbol(),
            "5"
        );
    }

    #[test]
    fn page_up_from_a_tall_transcript_renders_earlier_wrapped_rows() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 20,
            height: 10,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(TALL_MARKDOWN.into())));
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));
        app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageUp,
            crossterm::event::KeyModifiers::NONE,
        )));
        app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageUp,
            crossterm::event::KeyModifiers::NONE,
        )));
        let mut terminal = Terminal::new(TestBackend::new(20, 10)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        assert_eq!(
            terminal.backend().buffer().cell((0, 1)).unwrap().symbol(),
            "3"
        );
    }

    #[test]
    fn streamed_content_keeps_a_manually_scrolled_row_visible() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 20,
            height: 10,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(TALL_MARKDOWN.into())));
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));
        for _ in 0..2 {
            app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::PageUp,
                crossterm::event::KeyModifiers::NONE,
            )));
        }
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta("  \n10  \n11".into())));
        let mut terminal = Terminal::new(TestBackend::new(20, 10)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        assert_eq!(
            terminal.backend().buffer().cell((0, 1)).unwrap().symbol(),
            "3"
        );
    }

    #[test]
    fn narrow_spaced_words_follow_paragraph_wrap_geometry() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 12,
            height: 10,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
            "aaaaa 0 aaaaa 1 aaaaa 2 bbbbb 3 bbbbb 4 bbbbb 5 ccccc 6 ccccc 7 ccccc 8".into(),
        )));
        let mut terminal = Terminal::new(TestBackend::new(12, 10)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        for x in 0..5 {
            assert_eq!(buffer.cell((x, 1)).unwrap().symbol(), "b");
        }
        assert_eq!(buffer.cell((5, 1)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((6, 1)).unwrap().symbol(), "4");

        app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageUp,
            crossterm::event::KeyModifiers::NONE,
        )));
        terminal.draw(|frame| render(frame, &app)).unwrap();

        assert_eq!(
            terminal.backend().buffer().cell((6, 1)).unwrap().symbol(),
            "3"
        );
    }

    #[test]
    fn markdown_render_hides_fences_and_formats_quotes() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 60,
            height: 20,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(
            "> quoted\n\n```rust\nfn main() {}\n```\n\n| A | B |\n|---|---|\n| 1 | 2 |".into(),
        )));
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!rendered.contains("```"));
        assert!(rendered.contains("quoted"));
        assert!(rendered.contains("fn main() {}"));
        assert!(rendered.contains('┌'));
        assert!(rendered.contains('│'));
        for cell in ["A", "B", "1", "2"] {
            assert!(rendered.contains(cell));
        }
        let quote = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .find(|cell| cell.symbol() == "q")
            .unwrap();
        assert_ne!(quote.style(), ratatui::style::Style::default());
        let code = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .find(|cell| cell.symbol() == "f")
            .unwrap();
        assert_ne!(code.style(), ratatui::style::Style::default());
    }
}
