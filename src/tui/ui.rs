use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::app::{App, TurnState};
use super::support::render::renderable::Renderable;
use super::SYSTEM_COLOR;

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < 12 || area.height < 6 {
        frame.render_widget(Paragraph::new("July"), area);
        return;
    }

    // Header, transcript, composer. The composer draws its own footer hints, so July no longer
    // keeps a row of its own below it.
    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
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

    frame.render_widget(
        Paragraph::new(app.transcript_text())
            .wrap(Wrap { trim: false })
            .scroll((app.transcript_scroll(), 0)),
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

    use super::render;

    const TALL_MARKDOWN: &str = "0  \n1  \n2  \n3  \n4  \n5  \n6  \n7  \n8  \n9";

    fn row(terminal: &Terminal<TestBackend>, y: u16) -> String {
        (0..terminal.backend().buffer().area.width)
            .map(|x| terminal.backend().buffer().cell((x, y)).unwrap().symbol())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    /// First row of the transcript viewport.
    ///
    /// The layout is header, transcript, composer, footer; only the header is above the transcript.
    const TRANSCRIPT_TOP: u16 = 1;

    /// The row the composer's draft sits on, for a terminal `height` rows tall.
    ///
    /// Derived rather than hard-coded: the composer decides how tall it needs to be, and a test
    /// that pins that number breaks every time its footer hints change.
    fn draft_row(app: &App, height: u16) -> u16 {
        // The band ends at the bottom of the screen, and the composer insets its draft by one row.
        height - app.input_height() + 1
    }

    /// The rows the composer's band covers.
    fn composer_rows(app: &App, height: u16) -> std::ops::Range<u16> {
        (height - app.input_height())..height
    }

    /// The transcript rows currently on screen, blank rows dropped.
    ///
    /// The transcript viewport shrinks and grows with the composer, so tests say which rows are
    /// visible rather than pinning one to a fixed y.
    fn visible_transcript(terminal: &Terminal<TestBackend>, app: &App, height: u16) -> Vec<String> {
        (TRANSCRIPT_TOP..composer_rows(app, height).start)
            .map(|y| row(terminal, y))
            .filter(|line| !line.is_empty())
            .collect()
    }

    #[test]
    fn the_composer_footer_shows_an_error_over_the_default_hint() {
        fn footer(terminal: &Terminal<TestBackend>, app: &App, height: u16) -> String {
            composer_rows(app, height)
                .map(|y| row(terminal, y))
                .find(|line| line.contains("exit") || line.contains('!'))
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
            footer(&terminal, &app, 12).contains("/exit to leave"),
            "the default hint sits on the composer's own footer row"
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
        assert!(line.contains("! boom"), "an error replaces the hint: {line:?}");
        assert!(!line.contains("/exit"), "{line:?}");
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

        let mut app = App::new(
            Context::root().with_commands(vec!["/status".into(), "/start".into()]),
        );
        app.reduce(AppEvent::Resize {
            width: 72,
            height: 16,
        });
        app.reduce(AppEvent::Agents(vec![
            "cashpoint".into(),
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
        type_text(&mut app, "@cash");
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let mention = screen(&terminal);
        assert!(mention.contains("cashpoint"), "mention popup:\n{mention}");
        assert!(mention.contains("cashflow"), "mention popup:\n{mention}");
        assert!(
            mention.contains("Agent"),
            "agents are labelled:\n{mention}"
        );
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
        let cashpoint = AgentId::from(ulid::Ulid::from(1u128));
        let pay = AgentId::from(ulid::Ulid::from(2u128));
        for (agent, label) in [(cashpoint, "cashpoint"), (pay, "pay")] {
            app.reduce(AppEvent::AgentStreamStarted {
                agent,
                label: label.to_owned(),
            });
        }
        app.reduce(AppEvent::AgentStreamDelta {
            agent: cashpoint,
            delta: "Checking callback handler...".into(),
        });
        app.reduce(AppEvent::AgentStreamDelta {
            agent: pay,
            delta: "Inspecting refund state...".into(),
        });
        let mut terminal = Terminal::new(TestBackend::new(40, 16)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let rendered: Vec<String> = (0..16)
            .map(|y| row(&terminal, y))
            .filter(|line| !line.is_empty())
            .collect();
        let screen = rendered.join("\n");
        for expected in [
            "cashpoint",
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

        terminal.draw(|frame| render(frame, &app)).unwrap();

        let buffer = terminal.backend().buffer();
        let draft = draft_row(&app, 12);
        // The draft sits one row inside the band, behind the composer's own `› ` prompt.
        assert_eq!(buffer.cell((0, draft)).unwrap().symbol(), "›");
        assert_eq!(buffer.cell((2, draft)).unwrap().symbol(), "a");
        // The composer paints its own surface, so the whole band reads as one block rather than
        // the draft row sitting on bare terminal background.
        let surface = buffer.cell((0, draft)).unwrap().bg;
        for y in composer_rows(&app, 12) {
            assert_eq!(buffer.cell((0, y)).unwrap().bg, surface, "row {y}");
            assert_eq!(buffer.cell((1, y)).unwrap().bg, surface, "row {y}");
        }
        // The band is padded above and below the draft.
        assert!(draft > composer_rows(&app, 12).start);
        assert!(draft + 1 < composer_rows(&app, 12).end);
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
        // The header and at least one transcript row always survive.
        assert!(capped <= 12 - 2, "composer took {capped} of 12 rows");
        assert!(composer_rows(&app, 12).start > TRANSCRIPT_TOP);

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

        // The viewport is pinned to the tail: the newest row is on screen and the oldest is not.
        let visible = visible_transcript(&terminal, &app, 10);
        assert_eq!(visible.last().map(String::as_str), Some("9"), "{visible:?}");
        assert!(!visible.iter().any(|line| line == "0"), "{visible:?}");
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

        // Two rows back from the tail: the newest row has scrolled off and an earlier one is in.
        let visible = visible_transcript(&terminal, &app, 10);
        assert_eq!(visible.last().map(String::as_str), Some("8"), "{visible:?}");
        assert!(visible.iter().any(|line| line == "7"), "{visible:?}");
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
        let before = {
            let mut terminal = Terminal::new(TestBackend::new(20, 10)).unwrap();
            terminal.draw(|frame| render(frame, &app)).unwrap();
            visible_transcript(&terminal, &app, 10)
        };
        app.reduce(AppEvent::Chat(ChatEvent::TextDelta("  \n10  \n11".into())));
        let mut terminal = Terminal::new(TestBackend::new(20, 10)).unwrap();

        terminal.draw(|frame| render(frame, &app)).unwrap();

        // New content arriving must not drag the viewport away from where the user scrolled to.
        let after = visible_transcript(&terminal, &app, 10);
        assert_eq!(after, before, "the scrolled-to rows stayed put");
        assert!(!after.iter().any(|line| line == "11"), "{after:?}");
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

        // Wrapping follows paragraph geometry: a row breaks between words, so each one reads as a
        // whole "word digit" pair rather than being cut mid-word at column 12.
        let content = [
            "aaaaa 0", "aaaaa 1", "aaaaa 2", "bbbbb 3", "bbbbb 4", "bbbbb 5", "ccccc 6",
            "ccccc 7", "ccccc 8",
        ];
        let visible = visible_transcript(&terminal, &app, 10);
        assert!(
            visible.iter().all(|line| content.contains(&line.as_str())),
            "rows break between words: {visible:?}"
        );
        assert_eq!(visible.last().map(String::as_str), Some("ccccc 8"), "{visible:?}");
        let top_before = content
            .iter()
            .position(|line| Some(*line) == visible.first().map(String::as_str))
            .expect("the top row is one of the wrapped rows");

        app.reduce(AppEvent::Scroll { up: true, rows: 1 });
        terminal.draw(|frame| render(frame, &app)).unwrap();

        // Scrolling reveals the row before the one that was on top. The transcript puts a blank
        // spacer between rows, so a one-row scroll can uncover a row without hiding the tail.
        let scrolled = visible_transcript(&terminal, &app, 10);
        let top_after = content
            .iter()
            .position(|line| Some(*line) == scrolled.first().map(String::as_str))
            .expect("the top row is one of the wrapped rows");
        assert_eq!(top_after + 1, top_before, "{scrolled:?} vs {visible:?}");
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
