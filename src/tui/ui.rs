use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::SYSTEM_COLOR;
use super::app::{App, TurnState};
use super::support::render::renderable::Renderable;

pub fn render(frame: &mut Frame, app: &App) {
    // The frame is the bottom band july owns, not the whole screen: finished transcript rows live
    // in the terminal's scrollback, above this.
    let area = frame.area();
    if area.width < 12 || area.height < 2 {
        frame.render_widget(Paragraph::new("July"), area);
        return;
    }

    // Header, the rows still being streamed, then the composer. The composer draws its own footer
    // hints, so July no longer keeps a row of its own below it.
    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(app.input_height()),
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

    // What is still streaming: finished rows have already gone to the terminal's scrollback, so
    // this region is short. It is pinned to its tail, which is where the new text lands.
    let live = Paragraph::new(app.transcript_text()).wrap(Wrap { trim: false });
    let rows: u16 = live
        .line_count(areas[1].width.max(1))
        .try_into()
        .unwrap_or(u16::MAX);
    frame.render_widget(
        live.scroll((rows.saturating_sub(areas[1].height), 0)),
        areas[1],
    );

    // The composer insets its own draft, paints its own background and draws popups above it, so it
    // gets the whole band untouched.
    let input_area = areas[2];
    app.bottom_pane().render(input_area, frame.buffer_mut());
    // Ratatui shows the terminal cursor only for frames that place it, so an open modal - which
    // reports no cursor - leaves it hidden.
    if app.permission().is_none()
        && let Some((x, y)) = app.bottom_pane().cursor_pos(input_area)
    {
        frame.set_cursor_position((x, y));
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::application::ChatEvent;
    use crate::domain::PermissionOption;
    use crate::tui::app::{App, AppEvent, CommandResult, Context, ContextId};

    use crate::tui::support::style::user_message_bg;
    use crate::tui::support::terminal_palette::DefaultColors;
    use crate::tui::support::terminal_palette::with_test_default_colors;

    use super::render;

    fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
        (0..terminal.backend().buffer().area.width)
            .map(|x| terminal.backend().buffer().cell((x, y)).unwrap().symbol())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    /// The row the composer's draft sits on, for a band `height` rows tall.
    ///
    /// Derived rather than hard-coded: the composer decides how tall it needs to be, and a test
    /// that pins that number breaks every time its footer hints change.
    fn draft_row(app: &App, height: u16) -> u16 {
        // The composer ends at the bottom of the band and insets its draft by one row.
        height - app.input_height() + 1
    }

    /// The rows the composer's band covers.
    fn composer_rows(app: &App, height: u16) -> std::ops::Range<u16> {
        (height - app.input_height())..height
    }

    #[test]
    fn the_composer_footer_shows_an_error_over_the_default_hint() {
        fn footer(terminal: &Terminal<TestBackend>, app: &App, height: u16) -> String {
            composer_rows(app, height)
                .map(|y| row(terminal, y))
                .find(|line| line.contains('!'))
                .unwrap_or_default()
        }

        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        let mut app = App::new(Context::root().with_commands(vec!["/dm".into()]));
        app.reduce(AppEvent::Resize {
            width: 80,
            height: 12,
        });

        terminal.draw(|frame| render(frame, &app)).unwrap();
        assert!(
            footer(&terminal, &app, 12).is_empty(),
            "with nothing wrong the composer keeps its own footer row"
        );

        app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::NONE,
        )));
        app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        )));
        app.reduce(AppEvent::CommandFinished {
            context: ContextId::root(),
            result: CommandResult::Failed("boom".into()),
        });

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let line = footer(&terminal, &app, 12);
        assert!(
            line.contains("! boom"),
            "an error replaces the hint: {line:?}"
        );
    }

    /// Renders the whole composer surface end to end: draft, slash popup, mention popup.
    ///
    /// The composer is ported code with a lot of moving parts, so this checks what actually lands
    /// on screen rather than any one of its internals.
    #[test]
    fn composer_renders_draft_slash_popup_and_mention_popup() {
        fn screen(terminal: &Terminal<TestBackend>) -> String {
            let buffer = terminal.backend().buffer();
            (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer.cell((x, y)).unwrap().symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        fn type_text(app: &mut App, text: &str) {
            for character in text.chars() {
                app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char(character),
                    crossterm::event::KeyModifiers::NONE,
                )));
            }
            app.reduce(AppEvent::Tick);
        }
        fn clear(app: &mut App) {
            app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('c'),
                crossterm::event::KeyModifiers::CONTROL,
            )));
            app.reduce(AppEvent::Tick);
        }

        let mut app =
            App::new(Context::root().with_commands(vec!["/status".into(), "/start".into()]));
        app.reduce(AppEvent::Resize {
            width: 72,
            height: 16,
        });
        app.reduce(AppEvent::Agents(vec![
            "agent_order".into(),
            "cashflow".into(),
        ]));
        let mut terminal = Terminal::new(TestBackend::new(72, 16)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();
        let empty = screen(&terminal);
        assert!(empty.contains("Ask anything"), "placeholder:\n{empty}");
        assert!(
            !empty.contains("context left"),
            "July reports no token budget:\n{empty}"
        );

        type_text(&mut app, "hello world");
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let typed = screen(&terminal);
        assert!(typed.contains("› hello world"), "draft:\n{typed}");

        clear(&mut app);
        type_text(&mut app, "/st");
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let slash = screen(&terminal);
        assert!(slash.contains("/status"), "slash popup:\n{slash}");
        assert!(slash.contains("/start"), "slash popup:\n{slash}");

        clear(&mut app);
        // A bare `@` asks for every agent; a narrower query would only prove one of them renders.
        type_text(&mut app, "@");
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let mention = screen(&terminal);
        assert!(mention.contains("agent_order"), "mention popup:\n{mention}");
        assert!(mention.contains("cashflow"), "mention popup:\n{mention}");
        assert!(mention.contains("Agent"), "agents are labelled:\n{mention}");
    }

    /// Two agents streaming at once each get their own labelled block with a cursor.
    #[test]
    fn concurrent_agents_render_one_live_cell_each() {
        use crate::domain::AgentId;

        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 40,
            height: 16,
        });
        let agent_order = AgentId::from(ulid::Ulid::from(1u128));
        let pay = AgentId::from(ulid::Ulid::from(2u128));
        for (agent, label) in [(agent_order, "agent_order"), (pay, "pay")] {
            app.reduce(AppEvent::AgentStreamStarted {
                agent,
                label: label.to_owned(),
            });
        }
        app.reduce(AppEvent::AgentStreamDelta {
            agent: agent_order,
            delta: "Checking callback handler...".into(),
        });
        app.reduce(AppEvent::AgentStreamDelta {
            agent: pay,
            delta: "Inspecting refund state...".into(),
        });
        let screen = app.transcript_text_for_tests();
        for expected in [
            "agent_order",
            "Checking callback handler...",
            "pay",
            "Inspecting refund state...",
        ] {
            assert!(screen.contains(expected), "missing {expected:?}:\n{screen}");
        }
        assert_eq!(
            screen.matches('▌').count(),
            2,
            "one cursor per streaming agent:\n{screen}"
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
    fn the_command_popup_fits_inside_the_smallest_usable_layout() {
        let mut app = App::new(Context::root().with_commands(vec![
            "/alpha".into(),
            "/beta".into(),
            "/charlie".into(),
        ]));
        app.reduce(AppEvent::Resize {
            width: 16,
            height: 8,
        });
        app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('/'),
            crossterm::event::KeyModifiers::NONE,
        )));
        app.reduce(AppEvent::Tick);
        let mut terminal = Terminal::new(TestBackend::new(16, 8)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let screen: Vec<String> = (0..8).map(|y| row(&terminal, y)).collect();
        let joined = screen.join("\n");
        assert_eq!(screen[0], "● july (idle)");
        assert!(
            joined.contains("/alpha"),
            "the popup still lists commands at this size:\n{joined}"
        );
        assert!(
            screen[draft_row(&app, 8) as usize].starts_with('›'),
            "the draft is still visible:\n{joined}"
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

        // The surface the composer paints is derived from the terminal's own background, so the
        // test pins one rather than depending on what the terminal running the test reports.
        let terminal_bg = (0, 0, 0);
        with_test_default_colors(
            DefaultColors {
                fg: (204, 204, 204),
                bg: terminal_bg,
            },
            || terminal.draw(|frame| render(frame, &app)).unwrap(),
        );

        let buffer = terminal.backend().buffer();
        let draft = draft_row(&app, 12);
        // The draft sits one row inside the band, behind the composer's own `› ` prompt.
        assert_eq!(buffer.cell((0, draft)).unwrap().symbol(), "›");
        assert_eq!(buffer.cell((2, draft)).unwrap().symbol(), "a");
        // The composer paints its own surface, so the whole band reads as one block rather than
        // the draft row sitting on bare terminal background.
        let surface = buffer.cell((0, draft)).unwrap().bg;
        assert_eq!(
            surface,
            user_message_bg(terminal_bg),
            "the band lifts off the terminal background"
        );
        let band = composer_rows(&app, 12);
        // The footer hint row is the last of the band and sits outside the painted surface.
        for y in band.start..band.end - 1 {
            assert_eq!(buffer.cell((0, y)).unwrap().bg, surface, "row {y}");
            assert_eq!(buffer.cell((1, y)).unwrap().bg, surface, "row {y}");
        }
        assert_ne!(
            buffer.cell((0, band.end - 1)).unwrap().bg,
            surface,
            "the footer hint row stays on the terminal background"
        );
        // The band is padded above and below the draft.
        assert!(draft > band.start);
        assert!(draft + 1 < band.end);
    }

    #[test]
    fn the_composer_grows_until_it_would_crowd_out_the_transcript() {
        let mut app = App::new(Context::root());
        let mut terminal = Terminal::new(TestBackend::new(30, 12)).unwrap();
        app.reduce(AppEvent::Resize {
            width: 30,
            height: 12,
        });

        fn add_lines(app: &mut App, count: usize) {
            for _ in 0..count {
                app.reduce(AppEvent::Key(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Enter,
                    crossterm::event::KeyModifiers::ALT,
                )));
            }
        }

        add_lines(&mut app, 7);
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let capped = app.input_height();
        // The header and at least one scrollback row always survive.
        assert!(capped <= 12 - 2, "composer took {capped} of 12 rows");
        assert!(composer_rows(&app, 12).start > 0, "the header keeps row 0");

        add_lines(&mut app, 4);
        terminal.draw(|frame| render(frame, &app)).unwrap();

        assert_eq!(app.input_height(), capped, "it stops at the cap");
        assert!(
            row(&terminal, 0).starts_with('●'),
            "the header is never covered"
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

        // Two visual rows of text, plus the band's own padding and footer hint.
        assert_eq!(app.input_height(), 5);
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

        // The composer prints a two-column `› ` prompt, so the column the caret keeps when it moves
        // up maps two characters further into the text than the raw column would suggest.
        assert_eq!(app.input(), "abcXdefghijkl");
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

        let draft = draft_row(&app, 6);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((0, draft)).unwrap().symbol(), "›");
        assert_eq!(buffer.cell((2, draft)).unwrap().symbol(), "a");
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

        // Finished rows go to the terminal's scrollback, not the band, so the assertion is on the
        // rendered text rather than on the frame.
        let text = app.transcript_text();
        let rendered: String = text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect();
        assert!(!rendered.contains("```"));
        assert!(rendered.contains("quoted"));
        assert!(rendered.contains("fn main() {}"));
        assert!(rendered.contains('┌'));
        assert!(rendered.contains('│'));
        for cell in ["A", "B", "1", "2"] {
            assert!(rendered.contains(cell));
        }
        let styled = |needle: &str| {
            text.lines
                .iter()
                .find_map(|line| {
                    line.spans
                        .iter()
                        .find(|span| span.content.contains(needle))
                        .map(|span| line.style.patch(span.style))
                })
                .unwrap_or_default()
        };
        assert_ne!(styled("quoted"), ratatui::style::Style::default());
        assert_ne!(styled("fn main"), ratatui::style::Style::default());
    }

    #[test]
    fn a_permission_request_takes_over_the_pane_with_its_choices() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 44,
            height: 14,
        });
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
        let mut terminal = Terminal::new(TestBackend::new(44, 14)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let screen: String = (0..14)
            .map(|y| row(&terminal, y))
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "Permission requested",
            "Write file in src/main.rs?",
            "Allow once",
            "Reject",
        ] {
            assert!(screen.contains(expected), "missing {expected:?}:\n{screen}");
        }
        assert!(
            !screen.contains("Ask anything"),
            "the prompt covers the composer:\n{screen}"
        );
    }

    #[test]
    fn a_long_permission_prompt_still_shows_its_choice() {
        let mut app = App::new(Context::root());
        app.reduce(AppEvent::Resize {
            width: 32,
            height: 12,
        });
        app.reduce(AppEvent::Chat(ChatEvent::PermissionRequested {
            request_id: "permission-1".to_owned().into(),
            prompt: "This deliberately long permission prompt wraps across several rows in a narrow terminal before the only available choice".into(),
            options: vec![PermissionOption {
                id: "once".into(),
                label: "Allow once".into(),
            }],
        }));
        let mut terminal = Terminal::new(TestBackend::new(32, 12)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let screen: String = (0..12)
            .map(|y| row(&terminal, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            screen.contains("Allow once"),
            "a prompt long enough to wrap must not push its own choice off screen:\n{screen}"
        );
    }
}
