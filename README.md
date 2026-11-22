# Putio CLI - list file URLs.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](./LICENSE)

Implements a library for put.io API access as well as a CLI tool that lists
file download URLs. When a file in the current directory has the same name as
one on put.io, the local file is moved to `completed/` and the put.io copy is
deleted to free up allocated space.

## The token

Start by registering your application and obtaining your API credentials from
the [OAuth Apps page](https://app.put.io/settings/account/oauth/apps).

A put.io token does not expire and the v2 API exposes no scoping, so it is a
full-account credential: anything holding it can list, download and delete
everything in the account. It is never a command-line flag — that would put it
in `ps` output and in shell history for good — and it is sent only in an
`Authorization` header, never in a URL. `putio` looks for it in these places,
in this order, and uses the first that answers:

**1. A command** (`--token-command`, `$PUTIO_TOKEN_COMMAND`) — the first line
of its stdout. This is the one to reach for, because it keeps the token in
whatever you already trust with secrets:

```sh
putio <folder-id> --token-command 'pass putio/token'
putio <folder-id> --token-command 'gpg -qd ~/.putio.gpg'
putio <folder-id> --token-command 'op read op://private/putio/token'
```

It runs through the shell, and inherits stdin and stderr, so pinentry and other
prompts still work. A command that fails is an error — `putio` will not quietly
fall back to a weaker source you did not ask for.

**2. A file** (`--token-file`, `$PUTIO_TOKEN_FILE`) — the first line of a file
that must not be readable by group or others, the way ssh insists on a private
key. This is also how systemd hands a secret to a unit:

```ini
[Service]
LoadCredential=putio-token:/etc/putio/token
Environment=PUTIO_TOKEN_FILE=%d/putio-token
```

**3. `$PUTIO_TOKEN`** — what containers and CI want:

```sh
export PUTIO_TOKEN=<your token>
```

Convenient, and the loosest of the four. `export` in a shell profile leaves the
token in a plaintext dotfile and hands it to every child process — including the
`aria2c` in the pipelines below. On Linux it is not visible to other users
(`/proc/<pid>/environ` is readable only by its owner) and never shows up in
`ps`, but `docker inspect` prints a container's environment.

**4. `~/.config/putio/token`** (or `$XDG_CONFIG_HOME/putio/token`), mode 0600.
The no-arguments default:

```sh
install -m 600 /dev/null ~/.config/putio/token
printf '%s\n' '<your token>' > ~/.config/putio/token
```

**5. The OS keyring**, if built with `--features keyring` — Keychain on macOS,
Credential Manager on Windows, Secret Service (gnome-keyring, kwallet) on
Linux:

```sh
cargo install putio --features keyring
pass putio/token | putio --login   # reading from stdin keeps it out of history
putio --logout                     # forget it again
```

It comes last and is optional on purpose: a cron job or a container has no
keyring service to talk to, and a missing or locked keyring there falls through
to the message listing every source rather than a D-Bus error.

**If a token leaks**, deleting it locally is not enough — `putio --logout` does
not revoke anything. Revoke it on the
[OAuth Apps page](https://app.put.io/settings/account/oauth/apps) and issue a
new one.

In Docker, pass the token in at run time rather than baking it into an image:

```sh
docker run --rm --env-file ~/.config/putio/env putio <folder-id>
docker run --rm -v ~/.config/putio/token:/token:ro \
    -e PUTIO_TOKEN_FILE=/token putio <folder-id>
```

Note that the download URLs `putio` prints are themselves short-lived
credentials: anyone who has one can fetch the file without a token. They end up
in shell history and in `aria2c`'s command line, which is visible to other users
on a shared host.

## Usage

**List download URLs for every file in a folder:**

```sh
putio <folder-id from app.put.io/files/folder-id>
```

**Filter to a specific put.io file type** (e.g. only videos):

```sh
putio <folder-id> --file-type VIDEO
```

The accepted types are `VIDEO`, `AUDIO`, `IMAGE`, `ARCHIVE`, `PDF`, `TEXT` and
`FILE`; anything else is rejected before any request is made. `FOLDER` is not
one of them, because deleting a folder on put.io deletes everything inside it.
`--file-type` and `--delete-type` are mutually exclusive.

**Limit how many URLs are printed** (e.g. one URL at a time):

```sh
putio <folder-id> --file-type VIDEO --max-display 1
```

`--per-page` sets how many entries each put.io listing request asks for
(default 1). Raising it looks at more files per run; `--max-display` still caps
how many URLs are printed.

**Pipe the URLs to `aria2c` to download them:**

```sh
putio <folder-id> --file-type VIDEO \
    | xargs aria2c -x 5 --auto-file-renaming=false --allow-overwrite=false
```

**Download then clean up** — running the same command after `aria2c` finishes
detects the local files, deletes them from put.io, and moves them to `completed/`:

```sh
putio <folder-id> --file-type VIDEO \
    | xargs aria2c -x 5 --auto-file-renaming=false --allow-overwrite=false \
    && putio <folder-id> --file-type VIDEO
```

**Cleanup mode** — delete unwanted file types from put.io without downloading
anything (repeatable):

```sh
putio <folder-id> --delete-type ARCHIVE --delete-type IMAGE
```

**You can also combine this with the [Filesystem Librarian
tool](https://github.com/jasonrogena/librarian)** to move the downloaded files
to your preferred location:

```toml
[libraries.tvshows]
command = """
mkdir -p ./Done && mv "{{ file_path }}" ./Done/
"""

  [libraries.tvshows.filter]
  directories = [ "./completed" ]
  mime_type_regexs = [ "application/x-matroska" ]
```

Then chain everything together:

```sh
putio <folder-id> --file-type VIDEO \
    | xargs aria2c -x 5 --auto-file-renaming=false --allow-overwrite=false \
    && putio <folder-id> --file-type VIDEO \
    && fs-librarian single-shot librarian.toml
```
