//! MPD wire protocol: tokenizing, command parsing, response framing.
//!
//! Deliberately free of any WaveFlow types — this module turns bytes
//! into a [`Command`] and values back into bytes, nothing else. That
//! keeps the fiddly parts (quoting, ranges, ACK framing) unit-testable
//! without a Tauri app, a database or an audio engine anywhere in
//! sight.
//!
//! Reference: <https://mpd.readthedocs.io/en/latest/protocol.html>

/// Greeting written the instant a client connects, before it says
/// anything. The version is what clients feature-gate on; we answer
/// `0.23.0` because that is the oldest release exposing every command
/// we implement (notably `getvol`), so no client probes for something
/// we then have to ACK.
pub const GREETING: &[u8] = b"OK MPD 0.23.0\n";

/// ACK error codes. Only the ones we can actually emit are listed —
/// the full table is much longer and unused variants would just rot.
pub const ACK_ERROR_ARG: u32 = 2;
pub const ACK_ERROR_PASSWORD: u32 = 3;
pub const ACK_ERROR_PERMISSION: u32 = 4;
pub const ACK_ERROR_UNKNOWN: u32 = 5;
pub const ACK_ERROR_NO_EXIST: u32 = 50;

/// A position argument, which MPD spells either as a bare index
/// (`delete 3`) or a half-open range (`delete 3:7`). An open-ended
/// range (`3:`) means "to the end of the queue".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    Position(u32),
    /// `[start, end)` — end is exclusive, matching MPD.
    Bounded {
        start: u32,
        end: u32,
    },
    /// `start:` — from `start` to the end of the queue.
    From {
        start: u32,
    },
}

impl Range {
    /// Resolve against a queue length into an inclusive-exclusive pair
    /// clamped to `[0, len]`. Returns `None` when the range starts
    /// past the end or is inverted, which callers surface as
    /// `ACK_ERROR_ARG`.
    pub fn resolve(self, len: u32) -> Option<(u32, u32)> {
        let (start, end) = match self {
            Self::Position(p) => (p, p.saturating_add(1)),
            Self::Bounded { start, end } => (start, end),
            Self::From { start } => (start, len),
        };
        if start >= len || end <= start {
            return None;
        }
        Some((start, end.min(len)))
    }
}

/// Every command we answer. `Unknown` carries the verb so the ACK can
/// name it, which is what clients print to the user.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    // Connection
    Ping,
    Close,
    Password(String),
    // Reflection — clients call these on connect to decide what to show
    Commands,
    NotCommands,
    TagTypes,
    UrlHandlers,
    Decoders,
    Outputs,
    // State
    Status,
    CurrentSong,
    Stats,
    // Queue inspection
    PlaylistInfo(Option<Range>),
    PlaylistId(Option<u32>),
    // Transport
    Play(Option<u32>),
    PlayId(Option<u32>),
    Pause(Option<bool>),
    Stop,
    Next,
    Previous,
    /// `seek <pos> <seconds>`
    Seek(u32, f64),
    /// `seekid <id> <seconds>`
    SeekId(u32, f64),
    /// `seekcur <seconds>` — absolute, or relative when the argument
    /// carries an explicit `+`/`-` sign.
    SeekCur {
        seconds: f64,
        relative: bool,
    },
    // Mixer
    SetVol(u8),
    GetVol,
    /// `volume <delta>` — deprecated in MPD but still emitted by
    /// several remotes, so it is cheaper to support than to explain.
    VolumeDelta(i32),
    // Queue mutation
    Clear,
    Delete(Range),
    DeleteId(u32),
    Move {
        from: Range,
        to: u32,
    },
    MoveId {
        id: u32,
        to: u32,
    },
    Shuffle,
    // Options
    Random(bool),
    Repeat(bool),
    Single(SingleMode),
    Consume(bool),
    // Idle
    Idle(Vec<String>),
    NoIdle,
    Unknown(String),
}

/// `single` is tri-state since MPD 0.21: `0`, `1`, and `oneshot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingleMode {
    Off,
    On,
    OneShot,
}

/// One `key: value` line in a response. MPD is whitespace-sensitive
/// only in that the separator is exactly `": "`.
#[derive(Debug, Clone, PartialEq)]
pub struct Field(pub String, pub String);

/// A successful response body: an ordered list of fields. The trailing
/// `OK` is added by the connection layer, because inside a command list
/// it is replaced by `list_OK`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Response {
    pub fields: Vec<Field>,
}

impl Response {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, key: &str, value: impl ToString) {
        self.fields.push(Field(key.to_string(), value.to_string()));
    }

    /// Push only when the option is `Some`. MPD omits absent tags
    /// entirely rather than sending an empty value, and some clients
    /// render `Artist: ` as a literal blank artist if you don't.
    pub fn push_opt(&mut self, key: &str, value: Option<impl ToString>) {
        if let Some(v) = value {
            self.push(key, v);
        }
    }

    pub fn encode(&self) -> String {
        let mut out = String::new();
        for Field(k, v) in &self.fields {
            out.push_str(k);
            out.push_str(": ");
            // A newline inside a value would forge a protocol line, so
            // it is flattened rather than escaped: MPD has no escape
            // for it, and a tag with a newline is malformed anyway.
            out.push_str(&v.replace(['\n', '\r'], " "));
            out.push('\n');
        }
        out
    }
}

/// An `ACK` line. `list_index` is the 0-based position of the failing
/// command inside a command list (0 outside one).
#[derive(Debug, Clone, PartialEq)]
pub struct Ack {
    pub code: u32,
    pub list_index: u32,
    pub command: String,
    pub message: String,
}

impl Ack {
    pub fn new(code: u32, command: &str, message: impl Into<String>) -> Self {
        Self {
            code,
            list_index: 0,
            command: command.to_string(),
            message: message.into(),
        }
    }

    pub fn encode(&self) -> String {
        format!(
            "ACK [{}@{}] {{{}}} {}\n",
            self.code,
            self.list_index,
            self.command,
            self.message.replace(['\n', '\r'], " ")
        )
    }
}

/// Split a command line into verb + arguments, honouring MPD's quoting:
/// an argument may be wrapped in `"`, inside which `\"` and `\\` are
/// escapes. Unquoted arguments run to the next whitespace.
///
/// Returns `None` for a blank line, and treats an unterminated quote as
/// running to end-of-line rather than erroring — clients don't send
/// them, and being lenient here costs nothing.
pub fn tokenize(line: &str) -> Option<(String, Vec<String>)> {
    let mut tokens: Vec<String> = Vec::new();
    let mut chars = line.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut current = String::new();
        if c == '"' {
            chars.next();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => {
                        if let Some(escaped) = chars.next() {
                            current.push(escaped);
                        }
                    }
                    '"' => break,
                    _ => current.push(c),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                current.push(c);
                chars.next();
            }
        }
        tokens.push(current);
    }

    if tokens.is_empty() {
        return None;
    }
    let verb = tokens.remove(0);
    Some((verb, tokens))
}

fn parse_range(arg: &str) -> Option<Range> {
    if let Some((start, end)) = arg.split_once(':') {
        let start: u32 = start.parse().ok()?;
        if end.is_empty() {
            return Some(Range::From { start });
        }
        let end: u32 = end.parse().ok()?;
        Some(Range::Bounded { start, end })
    } else {
        Some(Range::Position(arg.parse().ok()?))
    }
}

/// MPD accepts `0`/`1` for booleans. Anything else is an argument error.
fn parse_bool(arg: &str) -> Option<bool> {
    match arg {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

/// Parse a whole command line. A malformed argument yields
/// `Command::Unknown(verb)` so the caller ACKs with the verb named —
/// the distinction between "no such command" and "bad argument" is
/// re-derived by the dispatcher, which knows which verbs exist.
pub fn parse(line: &str) -> Option<Command> {
    let (verb, args) = tokenize(line)?;
    let arg = |i: usize| args.get(i).map(String::as_str);

    let cmd = match verb.as_str() {
        "ping" => Command::Ping,
        "close" => Command::Close,
        "password" => Command::Password(arg(0).unwrap_or_default().to_string()),
        "commands" => Command::Commands,
        "notcommands" => Command::NotCommands,
        "tagtypes" => Command::TagTypes,
        "urlhandlers" => Command::UrlHandlers,
        "decoders" => Command::Decoders,
        "outputs" => Command::Outputs,
        "status" => Command::Status,
        "currentsong" => Command::CurrentSong,
        "stats" => Command::Stats,
        "playlistinfo" => Command::PlaylistInfo(match arg(0) {
            None => None,
            Some(a) => match parse_range(a) {
                Some(r) => Some(r),
                None => return Some(Command::Unknown(verb)),
            },
        }),
        "playlistid" => Command::PlaylistId(match arg(0) {
            None => None,
            Some(a) => match a.parse().ok() {
                Some(v) => Some(v),
                None => return Some(Command::Unknown(verb)),
            },
        }),
        "play" => Command::Play(match arg(0) {
            // `play -1` means "resume wherever we are" — it is how
            // several clients implement their play button, so it must
            // not be mistaken for a position.
            None | Some("-1") => None,
            Some(a) => match a.parse().ok() {
                Some(v) => Some(v),
                None => return Some(Command::Unknown(verb)),
            },
        }),
        "playid" => Command::PlayId(match arg(0) {
            None | Some("-1") => None,
            Some(a) => match a.parse().ok() {
                Some(v) => Some(v),
                None => return Some(Command::Unknown(verb)),
            },
        }),
        "pause" => Command::Pause(match arg(0) {
            None => None,
            Some(a) => match parse_bool(a) {
                Some(v) => Some(v),
                None => return Some(Command::Unknown(verb)),
            },
        }),
        "stop" => Command::Stop,
        "next" => Command::Next,
        "previous" => Command::Previous,
        "seek" => match (
            arg(0).and_then(|a| a.parse().ok()),
            arg(1).and_then(parse_seconds),
        ) {
            (Some(pos), Some(secs)) => Command::Seek(pos, secs),
            _ => return Some(Command::Unknown(verb)),
        },
        "seekid" => match (
            arg(0).and_then(|a| a.parse().ok()),
            arg(1).and_then(parse_seconds),
        ) {
            (Some(id), Some(secs)) => Command::SeekId(id, secs),
            _ => return Some(Command::Unknown(verb)),
        },
        "seekcur" => match arg(0) {
            Some(a) => match parse_seconds(a) {
                // A leading sign is what makes it relative; `seekcur 30`
                // is absolute, `seekcur +30` jumps forward half a minute.
                Some(secs) => Command::SeekCur {
                    seconds: secs,
                    relative: a.starts_with('+') || a.starts_with('-'),
                },
                None => return Some(Command::Unknown(verb)),
            },
            None => return Some(Command::Unknown(verb)),
        },
        "setvol" => match arg(0).and_then(|a| a.parse::<i32>().ok()) {
            Some(v) => Command::SetVol(v.clamp(0, 100) as u8),
            None => return Some(Command::Unknown(verb)),
        },
        "getvol" => Command::GetVol,
        "volume" => match arg(0).and_then(|a| a.parse::<i32>().ok()) {
            Some(v) => Command::VolumeDelta(v),
            None => return Some(Command::Unknown(verb)),
        },
        "clear" => Command::Clear,
        "delete" => match arg(0).and_then(parse_range) {
            Some(r) => Command::Delete(r),
            None => return Some(Command::Unknown(verb)),
        },
        "deleteid" => match arg(0).and_then(|a| a.parse().ok()) {
            Some(id) => Command::DeleteId(id),
            None => return Some(Command::Unknown(verb)),
        },
        "move" => match (
            arg(0).and_then(parse_range),
            arg(1).and_then(|a| a.parse().ok()),
        ) {
            (Some(from), Some(to)) => Command::Move { from, to },
            _ => return Some(Command::Unknown(verb)),
        },
        "moveid" => match (
            arg(0).and_then(|a| a.parse().ok()),
            arg(1).and_then(|a| a.parse().ok()),
        ) {
            (Some(id), Some(to)) => Command::MoveId { id, to },
            _ => return Some(Command::Unknown(verb)),
        },
        "shuffle" => Command::Shuffle,
        "random" => match arg(0).and_then(parse_bool) {
            Some(v) => Command::Random(v),
            None => return Some(Command::Unknown(verb)),
        },
        "repeat" => match arg(0).and_then(parse_bool) {
            Some(v) => Command::Repeat(v),
            None => return Some(Command::Unknown(verb)),
        },
        "single" => match arg(0) {
            Some("oneshot") => Command::Single(SingleMode::OneShot),
            Some("1") => Command::Single(SingleMode::On),
            Some("0") => Command::Single(SingleMode::Off),
            _ => return Some(Command::Unknown(verb)),
        },
        "consume" => match arg(0).and_then(parse_bool) {
            Some(v) => Command::Consume(v),
            None => return Some(Command::Unknown(verb)),
        },
        "idle" => Command::Idle(args),
        "noidle" => Command::NoIdle,
        _ => Command::Unknown(verb),
    };
    Some(cmd)
}

/// Seconds are a float in MPD, optionally signed for `seekcur`.
fn parse_seconds(arg: &str) -> Option<f64> {
    let value: f64 = arg.parse().ok()?;
    if value.is_finite() {
        Some(value)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_a_bare_command() {
        let (verb, args) = tokenize("status").unwrap();
        assert_eq!(verb, "status");
        assert!(args.is_empty());
    }

    #[test]
    fn tokenizes_quoted_arguments_with_spaces() {
        let (verb, args) = tokenize(r#"find artist "Tyler, The Creator""#).unwrap();
        assert_eq!(verb, "find");
        assert_eq!(args, vec!["artist", "Tyler, The Creator"]);
    }

    #[test]
    fn unescapes_inner_quotes_and_backslashes() {
        let (_, args) = tokenize(r#"cmd "a \"b\" c" "d\\e""#).unwrap();
        assert_eq!(args, vec![r#"a "b" c"#, r"d\e"]);
    }

    #[test]
    fn blank_line_is_not_a_command() {
        assert!(tokenize("   ").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn parses_a_bounded_range() {
        assert_eq!(
            parse("delete 3:7"),
            Some(Command::Delete(Range::Bounded { start: 3, end: 7 }))
        );
    }

    #[test]
    fn parses_an_open_ended_range() {
        assert_eq!(
            parse("delete 3:"),
            Some(Command::Delete(Range::From { start: 3 }))
        );
    }

    #[test]
    fn play_minus_one_means_resume_not_position() {
        // Several clients send `play -1` for their play button; reading
        // it as a position would jump to a nonexistent track.
        assert_eq!(parse("play -1"), Some(Command::Play(None)));
        assert_eq!(parse("play"), Some(Command::Play(None)));
        assert_eq!(parse("play 4"), Some(Command::Play(Some(4))));
    }

    #[test]
    fn seekcur_distinguishes_absolute_from_relative() {
        assert_eq!(
            parse("seekcur 30"),
            Some(Command::SeekCur {
                seconds: 30.0,
                relative: false
            })
        );
        assert_eq!(
            parse("seekcur +30"),
            Some(Command::SeekCur {
                seconds: 30.0,
                relative: true
            })
        );
        assert_eq!(
            parse("seekcur -10"),
            Some(Command::SeekCur {
                seconds: -10.0,
                relative: true
            })
        );
    }

    #[test]
    fn single_is_tri_state() {
        assert_eq!(parse("single 0"), Some(Command::Single(SingleMode::Off)));
        assert_eq!(parse("single 1"), Some(Command::Single(SingleMode::On)));
        assert_eq!(
            parse("single oneshot"),
            Some(Command::Single(SingleMode::OneShot))
        );
    }

    #[test]
    fn setvol_clamps_out_of_range_values() {
        assert_eq!(parse("setvol 150"), Some(Command::SetVol(100)));
        assert_eq!(parse("setvol -5"), Some(Command::SetVol(0)));
    }

    #[test]
    fn malformed_arguments_degrade_to_unknown() {
        assert_eq!(
            parse("setvol loud"),
            Some(Command::Unknown("setvol".into()))
        );
        assert_eq!(
            parse("repeat maybe"),
            Some(Command::Unknown("repeat".into()))
        );
        assert_eq!(parse("seek 1"), Some(Command::Unknown("seek".into())));
    }

    #[test]
    fn range_resolves_and_clamps_against_queue_length() {
        assert_eq!(Range::Position(2).resolve(5), Some((2, 3)));
        assert_eq!(
            Range::Bounded { start: 1, end: 99 }.resolve(5),
            Some((1, 5))
        );
        assert_eq!(Range::From { start: 2 }.resolve(5), Some((2, 5)));
        // Starting past the end, and an inverted range, are both errors.
        assert_eq!(Range::Position(9).resolve(5), None);
        assert_eq!(Range::Bounded { start: 4, end: 2 }.resolve(5), None);
        assert_eq!(Range::From { start: 5 }.resolve(5), None);
    }

    #[test]
    fn response_encodes_key_value_lines() {
        let mut r = Response::new();
        r.push("volume", 80);
        r.push_opt("Artist", Some("Radiohead"));
        r.push_opt("Album", None::<String>);
        assert_eq!(r.encode(), "volume: 80\nArtist: Radiohead\n");
    }

    #[test]
    fn response_flattens_newlines_so_a_tag_cannot_forge_a_line() {
        let mut r = Response::new();
        r.push("Title", "evil\nOK\nArtist: nope");
        // The injected newlines become spaces, so the client still sees
        // exactly one Title field.
        assert_eq!(r.encode(), "Title: evil OK Artist: nope\n");
    }

    #[test]
    fn ack_encodes_the_mpd_error_shape() {
        let mut ack = Ack::new(ACK_ERROR_UNKNOWN, "bogus", "unknown command \"bogus\"");
        ack.list_index = 2;
        assert_eq!(
            ack.encode(),
            "ACK [5@2] {bogus} unknown command \"bogus\"\n"
        );
    }
}
