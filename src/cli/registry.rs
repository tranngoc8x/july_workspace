//! The single source of truth for interactive (slash) commands.
//!
//! Parser, scope validation, help, and tests all read this table. A command
//! that is not registered here is not executable, and a registered command
//! without a handler is a registry invariant failure (see the tests below).

use std::fmt;

/// Explicit interactive scopes. There is deliberately no generic
/// `Conversation` scope: direct work and Work differ in what they expose.
/// `Thread` is the internal name of the scope users see as `work`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandScope {
    Root,
    Room,
    Dm,
    Thread,
}

impl CommandScope {
    pub const ALL: &'static [CommandScope] = &[Self::Root, Self::Room, Self::Dm, Self::Thread];

    pub fn label(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Room => "room",
            Self::Dm => "dm",
            Self::Thread => "work",
        }
    }
}

impl fmt::Display for CommandScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandKind {
    Navigation,
    Inspection,
    Control,
}

impl CommandKind {
    const ORDER: &'static [CommandKind] = &[Self::Navigation, Self::Inspection, Self::Control];

    fn heading(self) -> &'static str {
        match self {
            Self::Navigation => "Navigation",
            Self::Inspection => "Inspection",
            Self::Control => "Control",
        }
    }
}

pub struct CommandSpec {
    /// Canonical name including the leading slash, e.g. `/thread new`.
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub kind: CommandKind,
    pub scopes: &'static [CommandScope],
    pub summary: &'static str,
    pub usage: &'static str,
    pub examples: &'static [&'static str],
}

impl CommandSpec {
    pub fn available_in(&self, scope: CommandScope) -> bool {
        self.scopes.contains(&scope)
    }

    /// Number of whitespace-separated words in the canonical name.
    fn words(&self) -> usize {
        self.name.split_whitespace().count()
    }

    pub fn scope_error(&self, scope: CommandScope) -> String {
        let available: Vec<_> = self.scopes.iter().map(|scope| scope.label()).collect();
        format!(
            "{} is unavailable in {scope} context (available in: {})",
            self.name,
            available.join(", ")
        )
    }
}

use CommandKind::{Control, Inspection, Navigation};
use CommandScope::{Dm, Room, Thread};

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "/dm",
        aliases: &[],
        kind: Navigation,
        scopes: CommandScope::ALL,
        summary: "open a direct message with an agent",
        usage: "/dm <agent> [message]",
        examples: &["/dm cashpoint", "/dm @cashpoint what is still open?"],
    },
    CommandSpec {
        name: "/room",
        aliases: &[],
        kind: Navigation,
        scopes: CommandScope::ALL,
        summary: "enter a room",
        usage: "/room <room>",
        examples: &["/room vna"],
    },
    CommandSpec {
        name: "/thread new",
        aliases: &[],
        kind: Control,
        scopes: &[Room, Thread],
        summary: "create a thread in the current room",
        usage: "/thread new <title> [--goal <goal>]",
        examples: &[
            "/thread new \"Refund flow\"",
            "/thread new \"Refund flow\" --goal \"Implement refund API\"",
        ],
    },
    CommandSpec {
        name: "/thread",
        aliases: &[],
        kind: Navigation,
        scopes: &[Room, Thread],
        summary: "enter a thread of the current room",
        usage: "/thread <thread> [--agent <agent>]",
        examples: &["/thread 0198f0f2-0000-7000-8000-000000000001"],
    },
    CommandSpec {
        name: "/back",
        aliases: &[],
        kind: Navigation,
        scopes: CommandScope::ALL,
        summary: "pop the navigation history and restore the previous context",
        usage: "/back",
        examples: &["/back"],
    },
    CommandSpec {
        name: "/rooms",
        aliases: &[],
        kind: Inspection,
        scopes: CommandScope::ALL,
        summary: "list workspace rooms",
        usage: "/rooms",
        examples: &["/rooms"],
    },
    CommandSpec {
        name: "/agents",
        aliases: &[],
        kind: Inspection,
        scopes: CommandScope::ALL,
        summary: "list configured agents",
        usage: "/agents",
        examples: &["/agents"],
    },
    CommandSpec {
        name: "/deliveries",
        aliases: &[],
        kind: Inspection,
        scopes: CommandScope::ALL,
        summary: "list failed message deliveries",
        usage: "/deliveries",
        examples: &["/deliveries"],
    },
    CommandSpec {
        name: "/decisions",
        aliases: &[],
        kind: Inspection,
        scopes: CommandScope::ALL,
        summary: "list decisions waiting for a human",
        usage: "/decisions",
        examples: &["/decisions"],
    },
    CommandSpec {
        name: "/decision accept",
        aliases: &[],
        kind: Control,
        scopes: CommandScope::ALL,
        summary: "settle a user-owned decision",
        usage: "/decision accept <decision-id> --decision <text> [--reason <text>]",
        examples: &[
            "/decision accept 01ARZ3NDEKTSV4RRFFQ69G5FAV --decision keep current contract --reason tests pass",
        ],
    },
    CommandSpec {
        name: "/decision reject",
        aliases: &[],
        kind: Control,
        scopes: CommandScope::ALL,
        summary: "cancel a user-owned decision",
        usage: "/decision reject <decision-id> --reason <text>",
        examples: &["/decision reject 01ARZ3NDEKTSV4RRFFQ69G5FAV --reason request is obsolete"],
    },
    CommandSpec {
        name: "/decision work",
        aliases: &[],
        kind: Control,
        scopes: CommandScope::ALL,
        summary: "create explicit work from a decided decision",
        usage: "/decision work <decision-id> --work-id <work-id> --title <title> [--agent <agent>]",
        examples: &[
            "/decision work 01ARZ3NDEKTSV4RRFFQ69G5FAV --work-id 01ARZ3NDEKTSV4RRFFQ69G5FAA --title verify contract --agent codex",
        ],
    },
    CommandSpec {
        name: "/members",
        aliases: &[],
        kind: Inspection,
        scopes: &[Room, Thread],
        summary: "list active members of the current room or thread",
        usage: "/members",
        examples: &["/members"],
    },
    CommandSpec {
        name: "/work assign",
        aliases: &[],
        kind: Control,
        scopes: &[Thread],
        summary: "assign an explicit work item to an agent",
        usage: "/work assign <work-id> --agent <agent>",
        examples: &["/work assign 01ARZ3NDEKTSV4RRFFQ69G5FAV --agent cashpoint"],
    },
    CommandSpec {
        name: "/work status",
        aliases: &[],
        kind: Control,
        scopes: &[Thread],
        summary: "transition an explicit work item",
        usage: "/work status <work-id> <status>",
        examples: &["/work status 01ARZ3NDEKTSV4RRFFQ69G5FAV working"],
    },
    CommandSpec {
        name: "/work result",
        aliases: &[],
        kind: Control,
        scopes: &[Thread],
        summary: "record a result for an explicit work item",
        usage: "/work result <work-id> --status <status> --summary <summary>",
        examples: &[
            "/work result 01ARZ3NDEKTSV4RRFFQ69G5FAV --status accepted --summary refund contract verified",
        ],
    },
    CommandSpec {
        name: "/work",
        aliases: &[],
        kind: Navigation,
        scopes: &[Room, Thread],
        summary: "list work in the current room, or open one; inside work, list its items",
        usage: "/work [work]",
        examples: &["/work", "/work 0198f0f2-0000-7000-8000-000000000001"],
    },
    CommandSpec {
        name: "/results",
        aliases: &[],
        kind: Inspection,
        scopes: &[Thread],
        summary: "list results produced by the current thread",
        usage: "/results",
        examples: &["/results"],
    },
    CommandSpec {
        name: "/status",
        aliases: &[],
        kind: Inspection,
        scopes: CommandScope::ALL,
        summary: "show the current context",
        usage: "/status",
        examples: &["/status"],
    },
    CommandSpec {
        name: "/publish",
        aliases: &[],
        kind: Control,
        scopes: &[Thread],
        summary: "publish a result to the downstream thread",
        usage: "/publish <result> [--to <thread>]",
        examples: &[
            "/publish 0198f0f2-0000-7000-8000-000000000002",
            "/publish 0198f0f2-0000-7000-8000-000000000002 --to 0198f0f2-0000-7000-8000-000000000003",
        ],
    },
    CommandSpec {
        name: "/restart",
        aliases: &[],
        kind: Control,
        scopes: &[Dm, Thread],
        summary: "restart the current conversation's agent session",
        usage: "/restart",
        examples: &["/restart"],
    },
    CommandSpec {
        name: "/delivery retry",
        aliases: &[],
        kind: Control,
        scopes: CommandScope::ALL,
        summary: "retry one failed message delivery",
        usage: "/delivery retry <message-id> --agent <agent>",
        examples: &["/delivery retry 01ARZ3NDEKTSV4RRFFQ69G5FAV --agent codex"],
    },
    CommandSpec {
        name: "/help",
        aliases: &[],
        kind: Inspection,
        scopes: CommandScope::ALL,
        summary: "list available commands, or explain one",
        usage: "/help [command]",
        examples: &["/help", "/help thread"],
    },
    CommandSpec {
        name: "/exit",
        aliases: &["/quit"],
        kind: Control,
        scopes: CommandScope::ALL,
        summary: "leave the July REPL",
        usage: "/exit",
        examples: &["/exit"],
    },
];

/// Match `line` against the registry, longest canonical name first so
/// `/thread new` wins over `/thread`. Returns the spec and the remaining
/// argument text. Lines that do not start with `/` are never commands.
pub fn resolve(line: &str) -> Option<(&'static CommandSpec, &str)> {
    // A command typed with leading blanks is still a command; chat keeps the raw line.
    let line = line.trim_start();
    if !line.starts_with('/') {
        return None;
    }
    let mut best: Option<(&'static CommandSpec, &str)> = None;
    for spec in COMMANDS {
        for name in std::iter::once(spec.name).chain(spec.aliases.iter().copied()) {
            let Some(rest) = strip_command(line, name) else {
                continue;
            };
            if best.is_none_or(|(current, _)| spec.words() > current.words()) {
                best = Some((spec, rest));
            }
        }
    }
    best
}

/// `line` starts with `name` followed by end-of-line or whitespace.
fn strip_command<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(name)?;
    if rest.is_empty() {
        return Some(rest);
    }
    rest.starts_with(char::is_whitespace)
        .then(|| rest.trim_start())
}

/// Look up a command for `/help <command>`; the leading slash is optional.
pub fn find(name: &str) -> Option<&'static CommandSpec> {
    let name = name.trim();
    let slashed = if name.starts_with('/') {
        name.to_owned()
    } else {
        format!("/{name}")
    };
    COMMANDS
        .iter()
        .find(|spec| spec.name == slashed || spec.aliases.contains(&slashed.as_str()))
}

/// Still executable, no longer taught: Thread is an implementation detail of
/// Work, so `/work` is what `/help` points at. The handlers stay for scripts
/// and for anyone who already knows them.
const HIDDEN: &[&str] = &["/thread", "/thread new"];

pub fn for_scope(scope: CommandScope) -> impl Iterator<Item = &'static CommandSpec> {
    COMMANDS.iter().filter(move |spec| spec.available_in(scope))
}

/// `for_scope` minus the commands `/help` no longer advertises.
pub fn visible_for_scope(scope: CommandScope) -> impl Iterator<Item = &'static CommandSpec> {
    for_scope(scope).filter(|spec| !HIDDEN.contains(&spec.name))
}

/// `@` targets work, `/` runs a command: the two things a user must remember.
const HELP_PREAMBLE: &str = "\
Work with one agent
  @cashpoint fix callback retry

Work with several agents (inside a room)
  @cashpoint @pay implement refund flow

";

/// Context-aware `/help`, grouped by command kind.
pub fn help(scope: CommandScope) -> String {
    let mut rendered = String::from(HELP_PREAMBLE);
    for kind in CommandKind::ORDER {
        let mut group: Vec<_> = visible_for_scope(scope)
            .filter(|spec| spec.kind == *kind)
            .collect();
        if group.is_empty() {
            continue;
        }
        group.sort_by_key(|spec| spec.name);
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        rendered.push_str(kind.heading());
        rendered.push('\n');
        let width = group.iter().map(|spec| spec.usage.len()).max().unwrap_or(0);
        for spec in group {
            rendered.push_str(&format!(
                "  {:width$}  {}\n",
                spec.usage,
                spec.summary,
                width = width
            ));
        }
    }
    rendered
}

/// Detailed `/help <command>`, generated from the same metadata.
pub fn help_command(spec: &CommandSpec) -> String {
    let scopes: Vec<_> = spec.scopes.iter().map(|scope| scope.label()).collect();
    let mut rendered = format!(
        "{}\n  {}\n\nusage\n  {}\n\ncontexts\n  {}\n",
        spec.name,
        spec.summary,
        spec.usage,
        scopes.join(", ")
    );
    if !spec.aliases.is_empty() {
        rendered.push_str(&format!("\naliases\n  {}\n", spec.aliases.join(", ")));
    }
    if !spec.examples.is_empty() {
        rendered.push_str("\nexamples\n");
        for example in spec.examples {
            rendered.push_str(&format!("  {example}\n"));
        }
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_command_has_summary_usage_and_scopes() {
        for spec in COMMANDS {
            assert!(spec.name.starts_with('/'), "{} lacks a slash", spec.name);
            assert!(
                !spec.summary.trim().is_empty(),
                "{} lacks summary",
                spec.name
            );
            assert!(
                spec.usage.starts_with(spec.name),
                "{} usage must start with its name",
                spec.name
            );
            assert!(!spec.scopes.is_empty(), "{} lacks scopes", spec.name);
            assert!(!spec.examples.is_empty(), "{} lacks examples", spec.name);
        }
    }

    #[test]
    fn canonical_names_and_aliases_are_unique() {
        let mut seen = HashSet::new();
        for spec in COMMANDS {
            assert!(seen.insert(spec.name), "duplicate command {}", spec.name);
            for alias in spec.aliases {
                assert!(seen.insert(alias), "duplicate alias {alias}");
            }
        }
    }

    #[test]
    fn resolve_prefers_the_longest_canonical_name() {
        let (spec, rest) = resolve("/thread new \"Refund flow\"").unwrap();
        assert_eq!(spec.name, "/thread new");
        assert_eq!(rest, "\"Refund flow\"");

        let (spec, rest) = resolve("/thread abc").unwrap();
        assert_eq!(spec.name, "/thread");
        assert_eq!(rest, "abc");
    }

    #[test]
    fn resolve_requires_a_word_boundary_and_a_slash_after_leading_blanks() {
        assert!(resolve("/threading abc").is_none());
        assert_eq!(resolve("  /status").unwrap().0.name, "/status");
        assert!(resolve("hello").is_none());
        assert_eq!(resolve("/exit").unwrap().0.name, "/exit");
        assert_eq!(resolve("/quit").unwrap().0.name, "/exit");
    }

    #[test]
    fn find_accepts_bare_and_slashed_names() {
        assert_eq!(find("thread").unwrap().name, "/thread");
        assert_eq!(find("/publish").unwrap().name, "/publish");
        assert_eq!(find("quit").unwrap().name, "/exit");
        assert!(find("nope").is_none());
    }

    #[test]
    fn help_lists_only_commands_valid_in_the_scope() {
        let root = help(CommandScope::Root);
        assert!(root.contains("/dm <agent>"));
        assert!(!root.contains("/work"));

        let thread = help(Thread);
        assert!(thread.contains("/work"));
        assert!(thread.contains("/publish"));
    }

    #[test]
    fn visible_commands_are_canonical_scope_filtered_and_not_hidden() {
        let names = |scope| {
            visible_for_scope(scope)
                .map(|spec| spec.name)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            names(CommandScope::Root),
            [
                "/dm",
                "/room",
                "/back",
                "/rooms",
                "/agents",
                "/deliveries",
                "/decisions",
                "/decision accept",
                "/decision reject",
                "/decision work",
                "/status",
                "/delivery retry",
                "/help",
                "/exit"
            ]
        );
        assert_eq!(
            names(CommandScope::Room),
            [
                "/dm",
                "/room",
                "/back",
                "/rooms",
                "/agents",
                "/deliveries",
                "/decisions",
                "/decision accept",
                "/decision reject",
                "/decision work",
                "/members",
                "/work",
                "/status",
                "/delivery retry",
                "/help",
                "/exit"
            ]
        );
        assert_eq!(
            names(CommandScope::Dm),
            [
                "/dm",
                "/room",
                "/back",
                "/rooms",
                "/agents",
                "/deliveries",
                "/decisions",
                "/decision accept",
                "/decision reject",
                "/decision work",
                "/status",
                "/restart",
                "/delivery retry",
                "/help",
                "/exit"
            ]
        );
        assert_eq!(
            names(CommandScope::Thread),
            [
                "/dm",
                "/room",
                "/back",
                "/rooms",
                "/agents",
                "/deliveries",
                "/decisions",
                "/decision accept",
                "/decision reject",
                "/decision work",
                "/members",
                "/work assign",
                "/work status",
                "/work result",
                "/work",
                "/results",
                "/status",
                "/publish",
                "/restart",
                "/delivery retry",
                "/help",
                "/exit"
            ]
        );
        for scope in CommandScope::ALL {
            let visible = names(*scope);
            assert!(!visible.contains(&"/quit"));
            assert!(!visible.contains(&"/thread"));
            assert!(!visible.contains(&"/thread new"));
        }
    }

    #[test]
    fn help_teaches_mentions_and_no_longer_advertises_threads() {
        let room = help(Room);
        assert!(room.contains("@cashpoint @pay implement refund flow"));
        assert!(room.contains("/work [work]"));
        assert!(!room.contains("/thread"));
        // Hidden is not removed: the commands still resolve and still explain.
        assert_eq!(resolve("/thread new x").unwrap().0.name, "/thread new");
        assert!(help_command(find("thread").unwrap()).contains("usage"));
    }

    #[test]
    fn detailed_help_reports_registry_metadata() {
        let rendered = help_command(find("exit").unwrap());
        assert!(rendered.contains("aliases\n  /quit"));
        assert!(rendered.contains("contexts\n  root, room, dm, work"));
    }
}
