//! Finding the put.io OAuth token, and holding it without leaking it.
//!
//! put.io tokens do not expire and the v2 API exposes no scoping, so the token
//! is a full-account credential.

use anyhow::{bail, Context, Result};
use log::debug;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A put.io OAuth token.
///
/// Not zeroized on drop: the process is short-lived, and the realistic leak is
/// a log line rather than a core dump.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Token(String);

impl Token {
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

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(***)")
    }
}

/// Find the token, trying each source below in turn.
///
/// There is deliberately no `--token` flag: that would put the token in `ps`
/// output and in shell history for good.
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
         \x20 {default_path} (mode 0600){keyring}\n\
         Get a token from https://app.put.io/settings/account/oauth/apps"
    )
}

fn from_command(command: &str) -> Result<Token> {
    debug!("Resolving the token from a token command");
    let output = shell(command)
        // So gpg's pinentry and op's biometric prompt can reach the terminal.
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

/// Refuse a token file others can read, the way ssh refuses a loose key.
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

/// `pass` and friends print the secret on the first line and notes after it.
fn first_line(s: &str) -> &str {
    s.lines().find(|line| !line.trim().is_empty()).unwrap_or("")
}

#[cfg(feature = "keyring")]
const KEYRING_SERVICE: &str = "putio";
#[cfg(feature = "keyring")]
const KEYRING_USER: &str = "oauth-token";

#[cfg(feature = "keyring")]
fn keyring_entry() -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .context("opening the OS keyring; is there a keyring service on this host?")
}

/// Every failure is `None`, because a cron job or a container has no keyring
/// service and should get the "no token found" message, not a D-Bus error.
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

/// Needs a real credential store, which a Linux CI runner has no reason to
/// have: `cargo test --features keyring -- --ignored` in a desktop session.
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

    #[test]
    fn parse_rejects_a_captured_error_message_and_does_not_quote_it() {
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
