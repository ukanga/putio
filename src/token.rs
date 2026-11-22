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
    bail!("{}", no_token_help())
}

/// What to tell someone who has not set a token up yet. Lists the sources in
/// the order [`resolve`] tries them.
fn no_token_help() -> String {
    let default_path = default_token_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.config/putio/token".to_string());
    format!(
        "no put.io token found. Set one up with any of these, tried in this order:\n\
         \x20 --token-command CMD   ($PUTIO_TOKEN_COMMAND), e.g. --token-command 'pass putio/token'\n\
         \x20 --token-file PATH     ($PUTIO_TOKEN_FILE)\n\
         \x20 $PUTIO_TOKEN\n\
         \x20 {default_path}       (mode 0600)\n\
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
