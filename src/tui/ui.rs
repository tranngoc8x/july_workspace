use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap};

use super::app::{App, INPUT_HORIZONTAL_MARGIN, INPUT_VERTICAL_MARGIN, PermissionModal, TurnState};
use super::{ERROR_COLOR, SYSTEM_COLOR};

const INPUT_BACKGROUND_COLOR: Color = Color::Rgb(48, 54, 61);

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < 12 || area.height < 6 {
        frame.render_widget(Paragraph::new("July"), area);
        return;
    }

    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(app.input_height()),
        Constraint::Length(1),
    ])
    .split(area);
    let (dot_color, turn_label) = match app.turn_state() {
        TurnState::Idle => (Color::Green, "idle"),
        TurnState::Active => (SYSTEM_COLOR, "working"),
        TurnState::Cancelling => (Color::Magenta, "cancelling"),
        TurnState::CancelAcknowledged => (Color::DarkGray, "cancelled"),
    };
    // ponytail: the dot and the transcript spinner already report turn state, so
    // the header carries no command output.
    let header = vec![
        Span::styled("● ", Style::default().fg(dot_color)),
        Span::raw(app.context().label().to_owned()),
        Span::styled(format!(" ({turn_label})"), Style::default().fg(dot_color)),
    ];
    frame.render_widget(Paragraph::new(Line::from(header)), areas[0]);

    frame.render_widget(
        Paragraph::new(app.transcript_text())
            .wrap(Wrap { trim: false })
            .scroll((app.transcript_scroll(), 0)),
        areas[1],
    );

    let input_area = areas[2].inner(Margin {
        horizontal: INPUT_HORIZONTAL_MARGIN,
        vertical: INPUT_VERTICAL_MARGIN,
    });
    frame.render_widget(
        Block::default().style(Style::default().bg(INPUT_BACKGROUND_COLOR)),
        areas[2],
    );
    frame.render_widget(app.input_widget(), input_area);
    let completions = app.completions();
    let completion_visible = !completions.is_empty();
    if completion_visible {
        let height = u16::try_from(completions.len())
            .unwrap_or(u16::MAX)
            .min(areas[1].height);
        let popup = Rect::new(
            areas[1].x,
            areas[1].bottom().saturating_sub(height),
            areas[1].width,
            height,
        );
        let mention = app.completion_is_mention();
        let items = completions.iter().copied().map(|candidate| {
            ListItem::new(if mention {
                format!("@{candidate}")
            } else {
                candidate.to_owned()
            })
        });
        let list = List::new(items)
            .highlight_symbol("❯ ")
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        let mut state = ListState::default();
        state.select(Some(app.completion_selected()));
        frame.render_widget(Clear, popup);
        frame.render_stateful_widget(list, popup, &mut state);
    }
    let footer = match app.error() {
        Some(error) => Line::from(Span::styled(
            format!("! {error}"),
            Style::default().fg(ERROR_COLOR),
        )),
        // While a `/` command or `@` mention is being typed, the footer explains selection.
        None if completion_visible => Line::from(Span::styled(
            "↑↓ select · Enter/Tab complete",
            Style::default().fg(Color::Cyan),
        )),
        None => Line::from("July workspace · /exit to leave"),
    };
    frame.render_widget(Paragraph::new(footer), areas[3]);

    if let Some(permission) = app.permission() {
        let width = area.width.saturating_sub(4).min(60);
        let height = area
            .height
            .saturating_sub(2)
            .min((permission.options().len() as u16).saturating_add(7))
            .max(5)
            .min(area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let mut lines = vec![Line::from(permission.prompt().to_owned()), Line::default()];
        lines.extend(
            permission
                .options()
                .iter()
                .enumerate()
                .map(|(index, option)| {
                    let marker = if index == permission.selected() {
                        "❯ "
                    } else {
                        "  "
                    };
                    let style = if index == permission.selected() {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    };
                    Line::from(Span::styled(format!("{marker}{}", option.label), style))
                }),
        );
        lines.push(Line::default());
        lines.push(Line::from("Enter choose · Esc reject · Ctrl-C cancel"));
        let block = Block::bordered().title("Permission requested");
        let inner = block.inner(popup);
        let scroll = permission_scroll(permission, &lines, inner);
        frame.render_widget(Clear, popup);
        frame.render_widget(block, popup);
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0)),
            inner,
        );
    }
}

fn permission_scroll(permission: &PermissionModal, lines: &[Line<'_>], area: Rect) -> u16 {
    let width = area.width.max(1);
    let viewport = usize::from(area.height.max(1));
    let total = Paragraph::new(Text::from(lines.to_vec()))
        .wrap(Wrap { trim: false })
        .line_count(width);
    let max_scroll = total.saturating_sub(viewport);
    let requested = if permission.follows_selection() {
        let selected_line = permission.selected().saturating_add(2);
        Paragraph::new(Text::from(lines[..=selected_line].to_vec()))
            .wrap(Wrap { trim: false })
            .line_count(width)
            .saturating_sub(viewport)
    } else {
        usize::from(permission.scroll()).saturating_mul(viewport)
    };
    requested.min(max_scroll).min(usize::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    use crate::application::ChatEvent;
    use crate::domain::PermissionOption;
    use crate::tui::ERROR_COLOR;
    use crate::tui::app::{App, AppEvent, CommandResult, Context, ContextId};

    use super::render;

    const TALL_MARKDOWN: &str = "0  \n1  \n2  \n3  \n4  \n5  \n6  \n7  \n8  \n9";

    fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
        (0..terminal.backend().buffer().area.width)
            .map(|x| terminal.backend().buffer().cell((x, y)).unwrap().symbol())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    #[test]
    fn completion_list_renders_commands_mentions_and_error_priority() {
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();

        let app = App::new(Context::root());
        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert_eq!(row(&terminal, 11), "July workspace · /exit to leave");

        let mut commands =
            App::new(Context::root().with_commands(vec!["/dm".into(), "/debug".into()]));
        commands.reduce(AppEvent::Resize {
            width: 80,
            height: 12,
        });
        for character in "/d".chars() {
            commands.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(character),
                crossterm::event::KeyModifiers::NONE,
            )));
        }
        terminal.draw(|frame| render(frame, &commands)).unwrap();
        assert_eq!(row(&terminal, 6), "❯ /dm");
        assert_eq!(row(&terminal, 7), "  /debug");
        assert_eq!(row(&terminal, 11), "↑↓ select · Enter/Tab complete");

        commands.reduce(AppEvent::Chat(ChatEvent::PermissionRequested {
            request_id: "completion-permission".to_owned().into(),
            prompt: "Allow?".into(),
            options: vec![PermissionOption {
                id: "once".into(),
                label: "Allow once".into(),
            }],
        }));
        terminal.draw(|frame| render(frame, &commands)).unwrap();
        assert_eq!(row(&terminal, 11), "July workspace · /exit to leave");

        let mut mentions = App::new(Context::root());
        mentions.reduce(AppEvent::Agents(vec![
            "cashpoint".into(),
            "cashflow".into(),
        ]));
        mentions.reduce(AppEvent::Resize {
            width: 80,
            height: 12,
        });
        for character in "@cash".chars() {
            mentions.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(character),
                crossterm::event::KeyModifiers::NONE,
            )));
        }
        terminal.draw(|frame| render(frame, &mentions)).unwrap();
        assert_eq!(row(&terminal, 6), "❯ @cashpoint");
        assert_eq!(row(&terminal, 7), "  @cashflow");

        let mut error =
            App::new(Context::root().with_commands(vec!["/dm".into(), "/status".into()]));
        error.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::NONE,
        )));
        error.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        )));
        error.reduce(AppEvent::CommandFinished {
            context: ContextId::root(),
            result: CommandResult::Failed("boom".into()),
        });
        for character in "/d".chars() {
            error.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(character),
                crossterm::event::KeyModifiers::NONE,
            )));
        }
        terminal.draw(|frame| render(frame, &error)).unwrap();
        assert_eq!(row(&terminal, 11), "! boom");
        assert!(!(1..11).any(|y| row(&terminal, y).contains("/dm")));
        assert_eq!(
            terminal.backend().buffer().cell((0, 11)).unwrap().fg,
            ERROR_COLOR
        );
    }

    #[test]
    fn tiny_terminal_render_is_bounded_and_keeps_the_july_label() {
        let app = App::new(Context::root());
        let mut terminal = Terminal::new(TestBackend::new(8, 2)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((0, 0)).unwrap().symbol(), "J");
    }

    #[test]
    fn completion_list_scrolls_inside_the_minimum_editor_layout() {
        let mut app = App::new(Context::root().with_commands(vec![
            "/alpha".into(),
            "/beta".into(),
            "/charlie".into(),
        ]));
        app.reduce(AppEvent::Resize {
            width: 12,
            height: 6,
        });
        app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('/'),
            crossterm::event::KeyModifiers::NONE,
        )));
        for _ in 0..2 {
            app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Down,
                crossterm::event::KeyModifiers::NONE,
            )));
        }
        let mut terminal = Terminal::new(TestBackend::new(12, 6)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        assert_eq!(row(&terminal, 0), "● july (idle");
        assert_eq!(row(&terminal, 1), "❯ /charlie");
        assert_eq!(
            terminal.backend().buffer().cell((1, 3)).unwrap().symbol(),
            "/"
        );
    }

    #[test]
    fn input_surface_has_horizontal_and_vertical_padding() {
        let mut app = App::new(Context::root());
        let mut terminal = Terminal::new(TestBackend::new(30, 12)).unwrap();
        app.reduce(AppEvent::Resize {
            width: 30,
            height: 12,
        });
        app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::NONE,
        )));

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(app.input_height(), 3);
        assert_eq!(buffer.cell((0, 9)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((1, 9)).unwrap().symbol(), "a");
        for y in 8..=10 {
            assert_eq!(buffer.cell((0, y)).unwrap().bg, Color::Rgb(48, 54, 61));
            assert_eq!(buffer.cell((1, y)).unwrap().bg, Color::Rgb(48, 54, 61));
        }
    }

    #[test]
    fn input_grows_past_five_rows_until_terminal_space_is_full() {
        let mut app = App::new(Context::root());
        let mut terminal = Terminal::new(TestBackend::new(30, 12)).unwrap();
        app.reduce(AppEvent::Resize {
            width: 30,
            height: 12,
        });

        for _ in 0..7 {
            app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::ALT,
            )));
        }
        terminal.draw(|frame| render(frame, &app)).unwrap();

        assert_eq!(app.input_height(), 9);
        assert_eq!(
            terminal.backend().buffer().cell((0, 2)).unwrap().bg,
            Color::Rgb(48, 54, 61)
        );

        for _ in 0..4 {
            app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::ALT,
            )));
        }
        terminal.draw(|frame| render(frame, &app)).unwrap();

        assert_eq!(app.input_height(), 9);
        assert_eq!(
            terminal.backend().buffer().cell((0, 2)).unwrap().bg,
            Color::Rgb(48, 54, 61)
        );
    }

    #[test]
    fn long_input_soft_wraps_and_grows() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 12,
            height: 10,
        });
        for character in "abcdefghijkl".chars() {
            app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(character),
                crossterm::event::KeyModifiers::NONE,
            )));
        }

        assert_eq!(app.input_height(), 4);
    }

    #[test]
    fn arrow_up_moves_within_a_soft_wrapped_line() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 12,
            height: 10,
        });
        for character in "abcdefghijkl".chars() {
            app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(character),
                crossterm::event::KeyModifiers::NONE,
            )));
        }
        let mut terminal = Terminal::new(TestBackend::new(12, 10)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();

        app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Up,
            crossterm::event::KeyModifiers::NONE,
        )));
        app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('X'),
            crossterm::event::KeyModifiers::NONE,
        )));

        assert_eq!(app.input(), "abXcdefghijkl");
    }

    #[test]
    fn minimum_layout_still_renders_the_editor() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 12,
            height: 6,
        });
        app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::NONE,
        )));
        let mut terminal = Terminal::new(TestBackend::new(12, 6)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        assert_eq!(
            terminal.backend().buffer().cell((1, 3)).unwrap().symbol(),
            "a"
        );
    }

    #[test]
    fn minimum_layout_bounds_the_permission_modal() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Chat(ChatEvent::PermissionRequested {
            request_id: "permission-small".to_owned().into(),
            prompt: "Allow?".into(),
            options: vec![PermissionOption {
                id: "once".into(),
                label: "Allow once".into(),
            }],
        }));
        let mut terminal = Terminal::new(TestBackend::new(12, 6)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
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

        // The five-row viewport starts at "7" and keeps the latest row visible.
        assert_eq!(
            terminal.backend().buffer().cell((0, 1)).unwrap().symbol(),
            "7"
        );
    }

    #[test]
    fn scrolling_up_from_a_tall_transcript_renders_earlier_wrapped_rows() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 20,
            height: 10,
        });
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta(TALL_MARKDOWN.into())));
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));
        for _ in 0..2 {
            app.reduce(AppEvent::Scroll { up: true, rows: 1 });
        }
        let mut terminal = Terminal::new(TestBackend::new(20, 10)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        assert_eq!(
            terminal.backend().buffer().cell((0, 1)).unwrap().symbol(),
            "6"
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
            app.reduce(AppEvent::Scroll { up: true, rows: 1 });
        }
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta("  \n10  \n11".into())));
        let mut terminal = Terminal::new(TestBackend::new(20, 10)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        assert_eq!(
            terminal.backend().buffer().cell((0, 1)).unwrap().symbol(),
            "6"
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
        app.reduce(AppEvent::Chat(ChatEvent::TurnCompleted));
        let mut terminal = Terminal::new(TestBackend::new(12, 10)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        for x in 0..5 {
            assert_eq!(buffer.cell((x, 1)).unwrap().symbol(), "b");
        }
        assert_eq!(buffer.cell((5, 1)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((6, 1)).unwrap().symbol(), "4");

        app.reduce(AppEvent::Scroll { up: true, rows: 1 });
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

    #[test]
    fn permission_choices_render_in_an_exclusive_centered_modal() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Chat(ChatEvent::PermissionRequested {
            request_id: "permission-1".to_owned().into(),
            prompt: "Write file in src/main.rs?".into(),
            options: vec![
                PermissionOption {
                    id: "once".into(),
                    label: "Allow once".into(),
                },
                PermissionOption {
                    id: "reject".into(),
                    label: "Reject".into(),
                },
            ],
        }));
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("Permission requested"));
        assert!(rendered.contains("Write file in src/main.rs?"));
        assert!(rendered.contains("❯ Allow once"));
        assert!(rendered.contains("  Reject"));
        assert!(rendered.contains("Enter choose · Esc reject · Ctrl-C"));
        assert!(rendered.contains("cancel"));
    }

    #[test]
    fn permission_page_scroll_uses_wrapped_rows_and_clamps_at_the_end() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 32,
            height: 10,
        });
        app.reduce(AppEvent::Chat(ChatEvent::PermissionRequested {
            request_id: "permission-1".to_owned().into(),
            prompt: "This deliberately long permission prompt wraps across several rows in a narrow terminal before the only available choice".into(),
            options: vec![PermissionOption {
                id: "once".into(),
                label: "Allow once".into(),
            }],
        }));
        for _ in 0..20 {
            app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::PageDown,
                crossterm::event::KeyModifiers::NONE,
            )));
        }
        let bottom_page = app.permission().unwrap().scroll();
        assert!(bottom_page > 0);
        app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageUp,
            crossterm::event::KeyModifiers::NONE,
        )));
        assert_eq!(app.permission().unwrap().scroll(), bottom_page - 1);
        app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageDown,
            crossterm::event::KeyModifiers::NONE,
        )));
        let mut terminal = Terminal::new(TestBackend::new(32, 10)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("❯ Allow once"));
        assert!(rendered.contains("Ctrl-C cancel"));
    }
}
