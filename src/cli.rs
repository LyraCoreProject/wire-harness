//! The standalone client's CLI surface: where the server it talks to, and the identity it talks
//! as, are configured — and the ONLY place either is allowed to come from (#244).
//!
//! Two rules this module exists to enforce:
//!
//! 1. **No fixture is baked in.** There is no default account, no default password and no default
//!    character. A harness that shipped `TEST`/`test123`/`Ginger` defaults only ran against one
//!    server — the one those rows exist on — and every scenario silently inherited that stack.
//!    Every run now names its own credentials, so "a reachable build-5875 server plus test
//!    credentials" is the whole requirement.
//! 2. **The password never touches argv.** It is read from stdin, never printed, and never
//!    included in any log line. A positional plaintext password is visible in `ps`, in shell
//!    history and in every CI log that echoes its command line.
//!
//! Endpoints are process-global ([`set_endpoints`]) rather than threaded through every scenario:
//! which server a process talks to is a property of the whole run, and the alternative was an
//! extra parameter on ~60 scenario functions and their four dispatchers. Unset, they resolve to
//! the vanilla defaults, so a library consumer that never calls [`set_endpoints`] behaves exactly
//! like a stock 1.12 client.

use std::io::Read;
use std::sync::OnceLock;

use anyhow::{bail, Result};
use wow_world_messages::vanilla::{Class, Race};

/// The vanilla logon (auth) tier port. Configurable via `--logon-port`; this is only the default.
pub const DEFAULT_LOGON_PORT: u16 = 3724;
/// The world tier port used when a server's realm list does not answer with a usable address.
pub const DEFAULT_WORLD_PORT: u16 = 8085;
/// Default host for both tiers — a locally running server.
pub const DEFAULT_HOST: &str = "127.0.0.1";

/// Which server this process talks to. `world_addr_override` wins over the realm-list answer,
/// which is what lets the client target a server whose realm list advertises an address that is
/// not reachable from here (containers, port forwards, a shard behind a tunnel).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoints {
    pub logon_addr: String,
    pub world_addr_override: Option<String>,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            logon_addr: format!("{DEFAULT_HOST}:{DEFAULT_LOGON_PORT}"),
            world_addr_override: None,
        }
    }
}

impl Endpoints {
    /// Build from the three configurable pieces. `world_port` set means "ignore the realm-list
    /// answer and connect to `host:world_port`".
    pub fn new(host: &str, logon_port: u16, world_port: Option<u16>) -> Self {
        Self {
            logon_addr: format!("{host}:{logon_port}"),
            world_addr_override: world_port.map(|p| format!("{host}:{p}")),
        }
    }

    /// The world address to actually connect to, given whatever the realm list answered.
    pub fn world_addr(&self, realm_answer: &str) -> String {
        match &self.world_addr_override {
            Some(a) => a.clone(),
            None if realm_answer.is_empty() => format!("{DEFAULT_HOST}:{DEFAULT_WORLD_PORT}"),
            None => realm_answer.to_string(),
        }
    }
}

static ENDPOINTS: OnceLock<Endpoints> = OnceLock::new();

/// Publish this run's endpoints. First call wins (a binary calls it once, from its arg parse);
/// later calls are ignored rather than fatal so a library embedder cannot be tripped by ordering.
pub fn set_endpoints(e: Endpoints) {
    let _ = ENDPOINTS.set(e);
}

/// This run's endpoints, or the vanilla defaults if nothing published any.
pub fn endpoints() -> &'static Endpoints {
    static FALLBACK: OnceLock<Endpoints> = OnceLock::new();
    ENDPOINTS
        .get()
        .unwrap_or_else(|| FALLBACK.get_or_init(Endpoints::default))
}

/// What the invocation asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Log in, enter the world, report what came back, exit. The generic acceptance test: it needs
    /// nothing from the server but a working 5875 implementation and valid credentials.
    Smoke,
    /// Run the named protocol scenario with `Invocation::args`.
    Scenario(String),
}

/// A fully parsed invocation, password included.
#[derive(Clone, Debug)]
pub struct Invocation {
    pub command: Command,
    pub endpoints: Endpoints,
    pub account: String,
    pub character: String,
    pub class: Class,
    pub race: Race,
    pub password: String,
    /// Passwords for the EXTRA accounts a multi-session scenario logs in as, in the order the
    /// scenario asks for them. Read from the 2nd..Nth line of stdin — never from argv.
    pub peer_passwords: Vec<String>,
    /// Positional arguments after the command (and after the scenario name).
    pub args: Vec<String>,
}

impl Invocation {
    /// The password for the `i`th EXTRA session a scenario opens, with a message that says exactly
    /// how to supply it if it is missing.
    pub fn peer_password(&self, i: usize) -> Result<&str> {
        self.peer_passwords
            .get(i)
            .map(String::as_str)
            .ok_or_else(|| {
                anyhow::anyhow!(
                "this scenario opens {} additional session(s); pass their password(s) as line {} \
                 of stdin (one password per line: line 1 = --account's, line 2 = the first peer's)",
                i + 1,
                i + 2
            )
            })
    }
}

pub const USAGE: &str = "\
vanilla-wire — headless build-5875 (WoW 1.12.1) wire client

Speaks the real protocol — SRP6 logon then the encrypted world session — so tests can drive CMSG
and assert on decoded SMSG. It is server-agnostic: anything that implements build 5875 will do.

USAGE:
  vanilla-wire smoke --host HOST --account USER --password-stdin --character NAME
  vanilla-wire scenario NAME [SCENARIO-ARGS...] --account USER --password-stdin [--character NAME]

TARGET:
  --host HOST         host of the server under test                    [127.0.0.1]
  --logon-port PORT   logon (auth) tier port                           [3724]
  --world-port PORT   world tier port. Set it to IGNORE the realm-list
                      answer and connect to --host:PORT instead        [realm-list answer]

IDENTITY:
  --account NAME      login account                                    [required]
  --character NAME    character to play                                [required by most scenarios]
  --class CLASS       class used if --character has to be created      [warrior]
  --race RACE         race used if --character has to be created       [human]
  --password-stdin    read the password from stdin. REQUIRED: there is no password flag, no
                      default account and no default password, and the password is never logged.
                      Line 1 is --account's password; any further lines are consumed, in order,
                      by scenarios that open a SECOND session (e.g. say-range's listener).

  -h, --help          this text

EXAMPLES:
  printf '%s' \"$PASS\" | vanilla-wire smoke --host 10.0.0.5 --account TESTER \\
      --character Tester --password-stdin
  printf '%s\\n%s' \"$PASS\" \"$PEER\" | vanilla-wire scenario say-range PeerChar PEERACCT \\
      --account TESTER --character Tester --password-stdin

A scenario name no family claims is a hard error, never a silent success — a renamed or
mistyped scenario must not read as a green run.
";

/// Everything except the password — separated out so argument handling is unit-testable without
/// a stdin to feed it.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedArgs {
    pub command: Command,
    pub endpoints: Endpoints,
    pub account: String,
    pub character: String,
    pub class: Class,
    pub race: Race,
    pub args: Vec<String>,
    pub password_stdin: bool,
}

/// Parse an argv tail (i.e. WITHOUT the program name).
///
/// Flags may appear anywhere; the first non-flag token is the command, and for `scenario` the
/// second is the scenario name. Everything else, in order, is a scenario argument — including
/// negative numbers, which scenarios pass as coordinates (`-8968`), because only a `--` prefix
/// starts a flag. A bare `--` makes every following token positional.
pub fn parse_args<I: IntoIterator<Item = String>>(argv: I) -> Result<ParsedArgs> {
    let mut host = DEFAULT_HOST.to_string();
    let mut logon_port = DEFAULT_LOGON_PORT;
    let mut world_port: Option<u16> = None;
    let mut account = String::new();
    let mut character = String::new();
    let mut class = Class::Warrior;
    let mut race = Race::Human;
    let mut password_stdin = false;
    let mut positional: Vec<String> = Vec::new();

    let mut it = argv.into_iter();
    let mut only_positional = false;
    while let Some(tok) = it.next() {
        if only_positional || !tok.starts_with("--") {
            if tok == "-h" {
                print!("{USAGE}");
                std::process::exit(0);
            }
            positional.push(tok);
            continue;
        }
        let mut value = || -> Result<String> {
            it.next()
                .ok_or_else(|| anyhow::anyhow!("{tok} needs a value\n\n{USAGE}"))
        };
        match tok.as_str() {
            "--" => only_positional = true,
            "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--password-stdin" => password_stdin = true,
            "--host" => host = value()?,
            "--logon-port" => logon_port = value()?.parse()?,
            "--world-port" => world_port = Some(value()?.parse()?),
            "--account" => account = value()?,
            "--character" => character = value()?,
            "--class" => class = parse_class(&value()?)?,
            "--race" => race = parse_race(&value()?)?,
            other => bail!("unknown flag {other}\n\n{USAGE}"),
        }
    }

    let mut positional = positional.into_iter();
    let command = match positional.next().as_deref() {
        Some("smoke") => Command::Smoke,
        Some("scenario") => match positional.next() {
            Some(name) => Command::Scenario(name),
            None => bail!("`scenario` needs a name — e.g. `scenario logout`\n\n{USAGE}"),
        },
        Some(other) => {
            bail!("unknown command {other:?} — expected `smoke` or `scenario`\n\n{USAGE}")
        }
        None => bail!("no command given\n\n{USAGE}"),
    };
    if account.is_empty() {
        bail!("--account is required — this client has no default account\n\n{USAGE}");
    }
    if !password_stdin {
        bail!(
            "--password-stdin is required — this client has no default password and takes none \
             on the command line\n\n{USAGE}"
        );
    }

    Ok(ParsedArgs {
        command,
        endpoints: Endpoints::new(&host, logon_port, world_port),
        account,
        character,
        class,
        race,
        args: positional.collect(),
        password_stdin,
    })
}

/// Parse `std::env::args()`, then read the password(s) from stdin, and publish the endpoints.
pub fn invocation() -> Result<Invocation> {
    let parsed = parse_args(std::env::args().skip(1))?;
    let (password, peer_passwords) = read_passwords_from_stdin()?;
    set_endpoints(parsed.endpoints.clone());
    Ok(Invocation {
        command: parsed.command,
        endpoints: parsed.endpoints,
        account: parsed.account,
        character: parsed.character,
        class: parsed.class,
        race: parsed.race,
        password,
        peer_passwords,
        args: parsed.args,
    })
}

/// Read the whole of stdin and split it into `(password, peer_passwords)`.
/// Trailing newlines are stripped, so both `printf '%s' "$p"` and `echo "$p"` work.
pub fn read_passwords_from_stdin() -> Result<(String, Vec<String>)> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(split_passwords(&buf))
}

/// The stdin split, factored out for testing (never touches a real stdin).
pub fn split_passwords(raw: &str) -> (String, Vec<String>) {
    let mut lines = raw.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    let first = lines.next().unwrap_or_default().to_string();
    // A trailing newline yields one empty final element; drop empties from the tail only, so an
    // empty MIDDLE line still fails loudly at login rather than silently shifting peer indices.
    let mut peers: Vec<String> = lines.map(str::to_string).collect();
    while peers.last().is_some_and(String::is_empty) {
        peers.pop();
    }
    (first, peers)
}

/// Class names as the CLI spells them.
pub fn parse_class(s: &str) -> Result<Class> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "warrior" => Class::Warrior,
        "paladin" => Class::Paladin,
        "hunter" => Class::Hunter,
        "rogue" => Class::Rogue,
        "priest" => Class::Priest,
        "shaman" => Class::Shaman,
        "mage" => Class::Mage,
        "warlock" => Class::Warlock,
        "druid" => Class::Druid,
        other => bail!("unknown --class {other:?} (warrior|paladin|hunter|rogue|priest|shaman|mage|warlock|druid)"),
    })
}

/// Race names as the CLI spells them. The (race, class) pair must be legal in 1.12 — the server
/// rejects e.g. a Human shaman at character creation.
pub fn parse_race(s: &str) -> Result<Race> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "human" => Race::Human,
        "dwarf" => Race::Dwarf,
        "nightelf" | "night-elf" => Race::NightElf,
        "gnome" => Race::Gnome,
        "orc" => Race::Orc,
        "undead" | "scourge" => Race::Undead,
        "tauren" => Race::Tauren,
        "troll" => Race::Troll,
        other => {
            bail!("unknown --race {other:?} (human|dwarf|nightelf|gnome|orc|undead|tauren|troll)")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn smoke_needs_only_a_target_and_credentials() {
        let p = parse_args(argv(
            "smoke --host 10.0.0.5 --account TESTER --character Tester --password-stdin",
        ))
        .unwrap();
        assert_eq!(p.command, Command::Smoke);
        assert_eq!(p.endpoints.logon_addr, "10.0.0.5:3724");
        assert_eq!(p.endpoints.world_addr_override, None);
        assert_eq!(p.account, "TESTER");
        assert_eq!(p.character, "Tester");
    }

    #[test]
    fn every_endpoint_piece_is_configurable() {
        let p = parse_args(argv(
            "smoke --host wow.example.com --logon-port 3725 --world-port 8086 \
             --account A --password-stdin",
        ))
        .unwrap();
        assert_eq!(p.endpoints.logon_addr, "wow.example.com:3725");
        // A world port overrides whatever the realm list answers with.
        assert_eq!(
            p.endpoints.world_addr("192.168.0.9:8085"),
            "wow.example.com:8086"
        );
    }

    #[test]
    fn without_a_world_port_the_realm_list_answer_is_honored() {
        let e = Endpoints::new("10.0.0.5", 3724, None);
        assert_eq!(e.world_addr("203.0.113.7:8085"), "203.0.113.7:8085");
        // …and an empty answer still yields something connectable.
        assert_eq!(
            e.world_addr(""),
            format!("{DEFAULT_HOST}:{DEFAULT_WORLD_PORT}")
        );
    }

    #[test]
    fn there_is_no_default_account_and_no_password_flag() {
        let err = parse_args(argv("smoke --password-stdin"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--account is required"), "{err}");
        let err = parse_args(argv("smoke --account A"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--password-stdin is required"), "{err}");
        // The old positional form (account password character) is gone, not silently accepted.
        let err = parse_args(argv("TEST test123 Ginger"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown command"), "{err}");
        // …and there is no flag that would take a password on the command line.
        let err = parse_args(argv("smoke --account A --password hunter2"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown flag --password"), "{err}");
    }

    #[test]
    fn scenario_args_pass_through_including_negative_coordinates() {
        let p = parse_args(argv(
            "scenario walkmelee -8968 -129 83.4 -8800 -129 83.4 oneway \
             --account A --character C --password-stdin",
        ))
        .unwrap();
        assert_eq!(p.command, Command::Scenario("walkmelee".into()));
        assert_eq!(p.args, argv("-8968 -129 83.4 -8800 -129 83.4 oneway"));
    }

    #[test]
    fn a_double_dash_makes_the_rest_positional() {
        let p = parse_args(argv(
            "scenario echo --account A --password-stdin -- --host --not-a-flag",
        ))
        .unwrap();
        assert_eq!(p.args, argv("--host --not-a-flag"));
        assert_eq!(
            p.endpoints.logon_addr,
            format!("{DEFAULT_HOST}:{DEFAULT_LOGON_PORT}")
        );
    }

    #[test]
    fn stdin_carries_the_primary_password_then_the_peers() {
        assert_eq!(split_passwords("hunter2"), ("hunter2".into(), vec![]));
        assert_eq!(split_passwords("hunter2\n"), ("hunter2".into(), vec![]));
        assert_eq!(split_passwords("hunter2\r\n"), ("hunter2".into(), vec![]));
        assert_eq!(
            split_passwords("a\nb\nc\n"),
            ("a".into(), vec!["b".to_string(), "c".to_string()])
        );
    }

    #[test]
    fn a_missing_peer_password_says_how_to_supply_it() {
        let inv = Invocation {
            command: Command::Scenario("say-range".into()),
            endpoints: Endpoints::default(),
            account: "A".into(),
            character: "C".into(),
            class: Class::Warrior,
            race: Race::Human,
            password: "p".into(),
            peer_passwords: vec![],
            args: vec![],
        };
        let err = inv.peer_password(0).unwrap_err().to_string();
        assert!(err.contains("line 2 of stdin"), "{err}");
    }

    #[test]
    fn class_and_race_names_round_trip() {
        assert_eq!(parse_class("Mage").unwrap(), Class::Mage);
        assert_eq!(parse_race("NightElf").unwrap(), Race::NightElf);
        assert!(
            parse_class("deathknight").is_err(),
            "no TBC/WotLK classes on build 5875"
        );
    }
}
