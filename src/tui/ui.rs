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

    let mut transcript = app.completed_lines().join("\n");
    if !app.stream().is_empty() {
        if !transcript.is_empty() {
            transcript.push('\n');
        }
        transcript.push_str(app.stream());
    }
    frame.render_widget(
        Paragraph::new(transcript)
            .wrap(Wrap { trim: false })
            .scroll((app.scroll_offset().min(u16::MAX.into()) as u16, 0)),
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

    use crate::tui::app::{App, Context};

    use super::render;

    #[test]
    fn tiny_terminal_render_is_bounded_and_keeps_the_july_label() {
        let app = App::new(Context::root());
        let mut terminal = Terminal::new(TestBackend::new(8, 2)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((0, 0)).unwrap().symbol(), "J");
    }
}
