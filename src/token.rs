//! Holding the put.io OAuth token without leaking it, and finding it.
//!
//! put.io tokens do not expire and the v2 API exposes no scoping, so the token
//! is a full-account credential: it can list, download and delete everything.
//! That shapes this module twice over. [`Token`] keeps the value out of
//! `Debug` output, and [`resolve`] prefers sources that keep it off disk in
//! plaintext over the convenience of an exported environment variable.

use anyhow::{bail, Context, Result};
use log::debug;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A put.io OAuth token.
///
/// The value is private and `Debug` is redacted, so a stray `{:?}` on a
/// [`Config`](crate::Config), a `dbg!`, a log line or a panic payload cannot
/// print the token. The single place that needs the real thing calls
/// [`Token::expose`], which is deliberately conspicuous at the call site.
///
/// Not zeroized on drop: the process is short-lived, `String` reallocation
/// means copies may survive anyway, and the realistic leak is a log line, not
/// a core dump. Redaction is what pays for itself here.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// Validate `raw` and wrap it. `source` names where it came from, for the
    /// error message.
    ///
    /// Surrounding whitespace is trimmed, because a token read from a file or
    /// from a command's stdout arrives with a trailing newline. Anything that
    /// could not go into an `Authorization` header verbatim is rejected here,
    /// so a captured error message or shell prompt fails with its source named
    /// instead of at the first request.
    ///
    /// Errors never quote the value.
    pub fn parse(raw: &str, source: &str) -> Result<Token> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            bail!("{} gave an empty put.io token", source);
        }
        if !trimmed.chars().all(|c| c.is_ascii_graphic()) {
            bail!(
                "{} gave a put.io token containing whitespace or non-printable \
                 characters, which cannot be sent in an HTTP header",
                source
            );
        }
        Ok(Token(trimmed.to_owned()))
    }

    /// The token itself, for building the `Authorization` header.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(***)")
    }
}

/// Find the token, trying each source in turn:
///
/// 1. `command` (`--token-command`, `$PUTIO_TOKEN_COMMAND`) — run through the
///    shell, first line of stdout. This is the one to reach for: it delegates
///    storage to `pass`, `gpg`, `op`, `bw`, `vault` or `sops` without this
///    crate knowing about any of them.
/// 2. `file` (`--token-file`, `$PUTIO_TOKEN_FILE`) — first line of a file that
///    must not be group- or world-readable. Also how systemd's
///    `LoadCredential=` hands a secret to a unit.
/// 3. `$PUTIO_TOKEN` — what containers and CI want.
/// 4. `$XDG_CONFIG_HOME/putio/token`, else `~/.config/putio/token`.
/// 5. The OS keyring, when built with the `keyring` feature. Last, because it
///    may unlock a keyring or prompt, and because there is no keyring service
///    to talk to in a cron job or a container.
///
/// There is deliberately no `--token` flag: that would put the token in `ps`
/// output and in shell history for good.
///
/// An explicitly named command or file that fails is an error rather than a
/// reason to fall through, since the user pointed at it on purpose; a missing
/// default config file is not.
pub fn resolve(command: Option<&str>, file: Option<&Path>) -> Result<Token> {
    if let Some(command) = command {
        return from_command(command);
    }
    if let Some(path) = file {
        return from_file(path);
    }
    if let Some(value) = std::env::var_os("PUTIO_TOKEN") {
        let value = value
            .into_string()
            .map_err(|_| anyhow::anyhow!("$PUTIO_TOKEN is not valid UTF-8"))?;
        if !value.trim().is_empty() {
            debug!("Using the token from $PUTIO_TOKEN");
            return Token::parse(&value, "$PUTIO_TOKEN");
        }
    }
    if let Some(path) = default_token_path() {
        if path.is_file() {
            return from_file(&path);
        }
        debug!("No token file at {}", path.display());
    }
    #[cfg(feature = "keyring")]
    if let Some(token) = from_keyring() {
        return Ok(token);
    }
    bail!("{}", no_token_help())
}

/// What to tell someone who has not set a token up yet. Lists the sources in
/// the order [`resolve`] tries them.
fn no_token_help() -> String {
    let default_path = default_token_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.config/putio/token".to_string());
    let keyring = if cfg!(feature = "keyring") {
        "\n  the OS keyring  (putio --login)"
    } else {
        ""
    };
    format!(
        "no put.io token found. Set one up with any of these, tried in this order:\n\
         \x20 --token-command CMD   ($PUTIO_TOKEN_COMMAND), e.g. --token-command 'pass putio/token'\n\
         \x20 --token-file PATH     ($PUTIO_TOKEN_FILE)\n\
         \x20 $PUTIO_TOKEN\n\
         \x20 {default_path}       (mode 0600){keyring}\n\
         Get a token from https://app.put.io/settings/account/oauth/apps"
    )
}

/// Run `command` through the shell and take the first line of its stdout.
///
/// stdin and stderr are inherited so `gpg`'s pinentry, `op`'s biometric prompt
/// and the like can still talk to the terminal. Only stdout is captured.
fn from_command(command: &str) -> Result<Token> {
    debug!("Resolving the token from a token command");
    let output = shell(command)
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("running token command `{}`", command))?;
    if !output.status.success() {
        bail!(
            "token command `{}` failed ({})",
            command,
            match output.status.code() {
                Some(code) => format!("exit status {}", code),
                None => "killed by a signal".to_string(),
            }
        );
    }
    let stdout = String::from_utf8(output.stdout)
        .with_context(|| format!("token command `{}` printed invalid UTF-8", command))?;
    Token::parse(first_line(&stdout), &format!("token command `{}`", command))
}

#[cfg(unix)]
fn shell(command: &str) -> Command {
    let mut shell = Command::new("sh");
    shell.arg("-c").arg(command);
    shell
}

#[cfg(windows)]
fn shell(command: &str) -> Command {
    let mut shell = Command::new("cmd");
    shell.arg("/C").arg(command);
    shell
}

/// Take the first line of `path`, which must not be readable by group or other.
fn from_file(path: &Path) -> Result<Token> {
    check_permissions(path)?;
    let contents = fs::read_to_string(path)
        .with_context(|| format!("reading token file {}", path.display()))?;
    debug!("Resolved the token from {}", path.display());
    Token::parse(
        first_line(&contents),
        &format!("token file {}", path.display()),
    )
}

/// Refuse a token file others can read, the way ssh refuses a loose private
/// key. A secret that anyone on the host can read is not stored, just filed.
#[cfg(unix)]
fn check_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(path)
        .with_context(|| format!("reading token file {}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        bail!(
            "token file {} is readable by group or others (mode {:04o}); run `chmod 600 {}`",
            path.display(),
            mode & 0o7777,
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

/// `$XDG_CONFIG_HOME/putio/token`, falling back to `~/.config/putio/token`.
/// `None` when neither variable is set, as in a bare container.
fn default_token_path() -> Option<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })?;
    Some(config_home.join("putio").join("token"))
}

/// The first non-blank line of `s`. `pass` and friends print the secret on the
/// first line and anything else after it.
fn first_line(s: &str) -> &str {
    s.lines().find(|line| !line.trim().is_empty()).unwrap_or("")
}

/// The service and account names of the keyring entry: Keychain on macOS,
/// Secret Service on *nix, Credential Manager on Windows.
#[cfg(feature = "keyring")]
const KEYRING_SERVICE: &str = "putio";
#[cfg(feature = "keyring")]
const KEYRING_USER: &str = "oauth-token";

#[cfg(feature = "keyring")]
fn keyring_entry() -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .context("opening the OS keyring; is there a keyring service on this host?")
}

/// Look the token up in the OS keyring.
///
/// Every failure is `None`, not an error: a missing entry, a locked keyring and
/// a host with no keyring service at all should all end in the "no token
/// found" message that lists every source, not in a D-Bus error from a cron
/// job that was never going to have a session bus.
#[cfg(feature = "keyring")]
fn from_keyring() -> Option<Token> {
    let entry = match keyring_entry() {
        Ok(entry) => entry,
        Err(e) => {
            debug!("No usable OS keyring: {:#}", e);
            return None;
        }
    };
    match entry.get_password() {
        Ok(secret) => match Token::parse(&secret, "the OS keyring") {
            Ok(token) => {
                debug!("Resolved the token from the OS keyring");
                Some(token)
            }
            Err(e) => {
                debug!("Ignoring the OS keyring entry: {:#}", e);
                None
            }
        },
        Err(keyring::Error::NoEntry) => {
            debug!("No put.io token in the OS keyring");
            None
        }
        Err(e) => {
            debug!("OS keyring lookup failed: {}", e);
            None
        }
    }
}

/// `putio --login`: store a token in the OS keyring, read from stdin.
///
/// Piping is the point — `pass putio/token | putio --login` never puts the
/// token in shell history. A terminal gets a warning that what it types is
/// echoed, since reading without echo would mean another dependency.
#[cfg(feature = "keyring")]
pub fn login() -> Result<()> {
    use std::io::{BufRead, IsTerminal, Read};

    let mut input = String::new();
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprintln!("Paste your put.io token and press Enter.");
        eprintln!("It is echoed to the terminal; `pass putio/token | putio --login` is not.");
        stdin
            .lock()
            .read_line(&mut input)
            .context("reading the token from the terminal")?;
    } else {
        stdin
            .lock()
            .read_to_string(&mut input)
            .context("reading the token from stdin")?;
    }

    let token = Token::parse(first_line(&input), "stdin")?;
    keyring_entry()?
        .set_password(token.expose())
        .context("storing the token in the OS keyring")?;
    eprintln!(
        "Stored the put.io token in the OS keyring as {}/{}.",
        KEYRING_SERVICE, KEYRING_USER
    );
    Ok(())
}

/// `putio --logout`: delete the token from the OS keyring.
///
/// This revokes nothing. A token that may have leaked has to be revoked at
/// <https://app.put.io/settings/account/oauth/apps>, because put.io tokens do
/// not expire on their own.
#[cfg(feature = "keyring")]
pub fn logout() -> Result<()> {
    match keyring_entry()?.delete_credential() {
        Ok(()) => {
            eprintln!("Deleted the put.io token from the OS keyring.");
            eprintln!(
                "It still works until you revoke it at \
                 https://app.put.io/settings/account/oauth/apps"
            );
            Ok(())
        }
        Err(keyring::Error::NoEntry) => {
            eprintln!("There was no put.io token in the OS keyring.");
            Ok(())
        }
        Err(e) => Err(e).context("deleting the token from the OS keyring"),
    }
}

/// What `--login` and `--logout` are for in a build without keyring support.
#[cfg(not(feature = "keyring"))]
const NO_KEYRING: &str = "this putio was built without OS keyring support. Rebuild with \
                          `cargo install putio --features keyring`, or keep the token in \
                          `pass`/`gpg`/`op` and use --token-command";

#[cfg(not(feature = "keyring"))]
pub fn login() -> Result<()> {
    bail!("{}", NO_KEYRING)
}

#[cfg(not(feature = "keyring"))]
pub fn logout() -> Result<()> {
    bail!("{}", NO_KEYRING)
}

/// A `--login` that stored a token must be readable by the next run, and a
/// `--logout` after it must leave nothing behind. Ignored by default: it writes
/// to the developer's real keyring, and it needs a credential store to exist —
/// Keychain, Credential Manager, or a running Secret Service provider such as
/// gnome-keyring or kwallet on Linux, without which it fails with
/// `NoDefaultStore`. Run it with `cargo test --features keyring -- --ignored`.
///
/// The case that matters on a host with no keyring service is covered by the
/// ordinary tests: [`from_keyring`] returns `None` there, so the chain falls
/// through to the "no token found" message instead of a D-Bus error.
#[cfg(feature = "keyring")]
#[cfg(test)]
mod keyring_tests {
    use super::*;

    #[test]
    #[ignore = "writes to the real OS keyring"]
    fn round_trip_through_the_keyring() -> Result<()> {
        let entry = keyring_entry()?;
        entry.set_password("token_from_keyring")?;
        let token = from_keyring().expect("the token just stored must be found");
        assert_eq!(token.expose(), "token_from_keyring");
        logout()?;
        assert!(from_keyring().is_none(), "logout must leave nothing behind");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the newtype: no `{:?}` can print the token.
    #[test]
    fn debug_is_redacted() {
        let token = Token::parse("s3cret", "a test").expect("parse");
        assert_eq!(format!("{:?}", token), "Token(***)");
        assert!(!format!("{:?}", token).contains("s3cret"));
    }

    #[test]
    fn parse_trims_trailing_newline() {
        let token = Token::parse("abc123\n", "a test").expect("parse");
        assert_eq!(token.expose(), "abc123");
    }

    #[test]
    fn parse_rejects_empty() {
        let err = Token::parse("   \n", "a test").expect_err("empty is not a token");
        assert!(err.to_string().contains("a test"), "{}", err);
    }

    /// A command that printed a message instead of a token must fail here, not
    /// at the first request, and the error must not quote the value.
    #[test]
    fn parse_rejects_embedded_whitespace() {
        let err = Token::parse("gpg: decryption failed", "a test")
            .expect_err("a sentence is not a token");
        assert!(err.to_string().contains("whitespace"), "{}", err);
        assert!(!err.to_string().contains("decryption"), "{}", err);
    }

    #[test]
    fn first_line_skips_leading_blanks() {
        assert_eq!(first_line("\n\nabc\ndef\n"), "abc");
        assert_eq!(first_line(""), "");
    }
}
