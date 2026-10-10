<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/rail-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="assets/brand/rail-light.png">
    <img alt="Rail. Modern email. Native to your terminal." src="assets/brand/rail-light.png" width="560">
  </picture>
</h1>

[![ci](https://github.com/vitosans/rail/actions/workflows/ci.yml/badge.svg)](https://github.com/vitosans/rail/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
![rust](https://img.shields.io/badge/rust-1.88%2B-orange.svg)

Rail is a Rust terminal mail client built on ratatui: a modern, fast homage to mutt with a local
store, a thread view, a unified inbox, readable HTML and a command palette. Gmail and Microsoft 365
first.

**Status: pre-alpha.** The setup wizard, login, config, store, `rail doctor`, the background daemon,
1Password storage and the MCP server for AI coding agents work. The mail views are not built yet:
with an account configured, `rail` still prints that the TUI is not built and exits with status 2.

## Why Rail

- **Threads are the unit.** The list shows conversations, and the reader collapses and expands them.
- **One inbox.** All accounts merge into a unified view with per-account colour and a quick filter.
- **Readable HTML.** HTML mail renders in the terminal, with one key to open it in a browser.
- **Everything is local.** Every list, scroll, search and open is a SQLite query. The UI never waits
  on the network.
- **Palette and keymap.** `:` fuzzy-matches every command, folder and account; `?` shows the keymap for
  the current view.

## Roadmap

| Phase | Scope | Status |
|---|---|---|
| 1 | Workspace, config, store schema, secrets, OAuth login, `rail account`, `rail doctor` | Done |
| 2 | App shell and onboarding: setup wizard, organisation presets, admin consent, `rail setup`, `rail account login` | Done |
| 3 | Daemon, operation layer and MCP server: `rail daemon`, `rail mcp`, guided provider setup, 1Password, `ai_send` | Done |
| 4 | Gmail provider and sync, inside the daemon: `rail sync` fills the store | Next |
| 5 | TUI read path: thread list, sidebar, reader, palette, help, filter | Planned |
| 6 | Actions through the outbox and the compose round-trip; agent sends under `ai_send` | Planned |
| 7 | Microsoft Graph provider and device-code flow | Planned |
| 8 | Polish: search operators, auto layout, theme, inline images | Planned |

The overall design is in [`docs/superpowers/specs/2026-10-09-rail-design.md`](docs/superpowers/specs/2026-10-09-rail-design.md);
the shell and the wizard are in [`docs/superpowers/specs/2026-10-09-rail-onboarding-design.md`](docs/superpowers/specs/2026-10-09-rail-onboarding-design.md),
and the daemon and the MCP server in [`docs/superpowers/specs/2026-10-10-rail-daemon-mcp-design.md`](docs/superpowers/specs/2026-10-10-rail-daemon-mcp-design.md).

## Install

Requires Rust 1.88 or newer.

    cargo install --path crates/rail

## Get started

Run Rail with no arguments:

    rail

With no config file yet, Rail opens the setup wizard. It asks how you want to set up the account:

- **Use a preset from my organisation.** Pick the preset your organisation installed, or type the
  path or `https://` URL it gave you. Enter your address, log in through the browser, done.
- **Set up my own app.** Choose Gmail or Microsoft 365. The wizard shows the portal steps (the same
  ones as in [Setup](#setup) below), opens the portal with `o`, and asks for the values the portal
  gave you. For Microsoft the client ID field is the app's *Application (client) ID*. The Gmail
  client secret field also takes an `op://` 1Password reference (see
  [Tokens in 1Password](#tokens-in-1password)).

The wizard ends with a report of where everything lives and offers to add another account.
`rail setup` opens it any time. `rail account add` with no arguments opens it too, and
`rail account add gmail` (or `m365`) opens it with that provider already chosen.

Keys:

| Key | Action |
|---|---|
| `↑` / `↓` | Move within a list. |
| `Tab` / `Shift-Tab` | Change focus between fields. |
| `Enter` | Choose or continue. |
| `Space` | Tick or untick the checkbox. |
| `Esc` | Go back one step. |
| `F1` | Open the provider portal, or the preset's help link. |
| `o` | On the Instructions step, open the provider portal in your browser. |
| `r` / `c` | `r` retries when an error is shown. `c` copies the admin-consent URL shown with an error, or the login URL on the Login step. |
| `q` | Quit, except while typing in a text field. |
| `Ctrl-C` | Quit from anywhere. While an account is being saved, `Esc` and `q` do nothing and `Ctrl-C` waits for the save to finish, then quits. |

The [background daemon](#the-background-daemon) saves the account as soon as the browser hands the
login back, and the wizard then shows where everything was saved. `Esc` or `Ctrl-C` while the
wizard waits for the browser cancels that login (or admin consent) in the daemon, so nothing is
left waiting there. A save the daemon has already started is never interrupted, so a half-written
account never happens: if you quit just as the browser came back, the daemon finishes the save and
Rail says so once the terminal is back (see below).

`c` copies with the terminal clipboard escape sequence (OSC 52), which some terminals ignore without
saying so; if nothing was copied, select the URL on screen. The login URL only works while Rail is
running, so open it before you quit (select it on screen if the browser did not open). If the wizard
ends before it finishes, Rail prints only the last admin-consent URL it showed once the terminal is
back, so a non-administrator can forward it to an administrator. The login URL is never printed.

When the wizard exits, the status is 0 if at least one account was saved (also when you quit after a
save; Rail then prints "setup interrupted; saved: <names>"), and 1 with "setup cancelled; nothing was
saved" otherwise. Quitting before the save of an account saves nothing for that account. Quitting
while the daemon was already saving one keeps it: Rail prints "setup cancelled; the account being
saved was kept — check `rail account list`" and exits 0.

The wizard needs an interactive terminal. When stdout is a pipe or a file, every way of opening it
(`rail`, `rail setup`, `rail account add` without flags) prints a one-line hint and exits with
status 2; use the flag form of `rail account add` instead.

## Use Rail from an AI agent (MCP)

Rail is also an [MCP](https://modelcontextprotocol.io) server, so a coding agent with a shell
(Claude Code, Codex, Cursor and similar) can do everything the wizard does, including setting up
the Google project or the Microsoft app registration. Register it once:

    claude mcp add rail -- rail mcp

(other clients: run the command `rail mcp` as a stdio server). Then ask the agent to run Rail's
`setup` prompt. It checks `rail_status`, asks which providers you want, and walks you through
`rail_provider_guide` one step at a time: it runs the `gcloud` or `az` commands with your
approval, hands you the browser for the pages only you can complete (the Google consent screen
and the desktop client download, and every login), and finishes with `rail_account_add` and
`rail_wait`. No secret passes through the agent: `rail_account_add` takes the absolute path of the
client secret JSON you downloaded, an `op://` reference or a preset, never the secret itself, and
tokens stay in the daemon. A `client_secret` sent by an agent is refused, and a value in
`client_secret_ref` that is not an `op://` reference is refused without being repeated anywhere,
in case it was a pasted secret. Every tool refuses arguments it does not know. File paths given to any
tool must be absolute (the daemon's working directory is not the agent's); a relative one is
refused with "use an absolute path".

| Tool | What it does |
|---|---|
| `rail_status` | Everything `rail doctor` checks, including each account's `ai_send` policy, plus the daemon's state. Read-only. |
| `rail_accounts_list` | Configured accounts, token presence, `ai_send` policy. Read-only. |
| `rail_presets_list` | Presets installed on this machine. Read-only. |
| `rail_preset_fetch` | A preset from an `https://` URL or an absolute file path. Read-only. |
| `rail_provider_guide` | The Google or Microsoft setup steps, as data. Read-only. |
| `rail_account_add` | Validates a new account, opens its login and returns the URL and an id. With `store_secret_in_1password` set, Rail keeps the client secret in 1Password (see below). |
| `rail_account_login` | Logs in again to a configured account. |
| `rail_admin_consent` | Microsoft admin consent for one organisation. |
| `rail_wait` | Waits, at most five minutes per call, for a login or consent to finish. Read-only. An agent giving up on a wait does not cancel the login. |
| `rail_cancel` | Cancels a login or consent that has not reached the save. |
| `rail_secrets_configure` | Moves tokens between the keyring, the file and 1Password (see below). Needs your confirmation. |
| `rail_account_remove` | Removes an account's config entry and tokens; its mail stays. Destructive; needs your confirmation. |

There are no tools that send mail yet.

`rail_account_remove` and `rail_secrets_configure` ask you to confirm in the agent's own dialog
(MCP elicitation), showing what changes: the account and what is removed, or the old and new
token store, the vault and how many accounts' tokens move (and, when the target is the file, that
tokens will sit in a plain 0600 file instead of the keyring or 1Password; when tokens are not
moved, that each account then needs `rail account login`). The daemon writes that text, with
every name, address, vault and 1Password account quoted and shown in plain ASCII (every
non-ASCII character as `\u{..}`, so a look-alike letter cannot pass for a Latin one) and never
cut, so none can fake a line, hide characters or look like another; vault and 1Password account
names over 100 characters, and addresses over 254, are refused instead. The text is bound to a
digest of the exact plan, the old token store included: the confirmed request must carry the
digest, and if anything changed in between (an account added, say), nothing runs and the agent
has to ask you again. Neither tool has an input that confirms for you. When you decline, dismiss the dialog or
leave its box unticked, nothing changes. A client that cannot show the dialog changes nothing
either: the agent is told to ask you to run `rail account remove <name>` in a terminal, or to
edit `secrets` (and `[onepassword]`) in `config.toml` yourself. A setting that is already in place
needs no confirmation.

Text from outside Rail, such as preset files, the mail provider's and the system's error messages
now and email later, comes back marked as untrusted. In the summaries the agent reads, every such
piece is quoted on one line, cut to 200 characters, and preceded by a notice that it is data,
never instructions. That lowers the risk of a message or a preset steering the agent; the
`ai_send` policy (see [Configuration](#what-an-ai-agent-may-do-with-your-mail-ai_send)) is the real
control for sending.

`rail mcp` writes only MCP messages to its standard output and logs to `rail.log`; it logs the
MCP library's own messages only at error level, since its warnings about malformed arguments
can quote what the agent sent. A more specific `RUST_LOG` target overrides that: with
`RUST_LOG=rmcp::handler=warn`, those warnings come back and may quote tool arguments, so do not
set one while an agent might be sending a pasted secret, and treat `rail.log` as sensitive then.
It talks to the background daemon like every other command.

## Commands

| Command | What it does |
|---|---|
| `rail` | Opens the setup wizard when there is no config yet. With a config it prints that the TUI is not built yet and exits with status 2. |
| `rail setup` | Opens the setup wizard to add an account. |
| `rail account add [<gmail\|m365>]` | With no arguments, or only a provider, opens the setup wizard. |
| `rail account add <gmail\|m365> --address <address> --client-id <id> [--client-secret <secret>] [--tenant <tenant>] [--name <name>] [--color <colour>]` | Adds an account without the wizard, for scripts. `--address` needs the provider and `--client-id`; any other flag needs `--address`. A partial set exits with status 1 and names the missing flag. `--client-secret` may be an `op://` 1Password reference (see [Tokens in 1Password](#tokens-in-1password)). |
| `rail account login <name-or-address>` | Logs in again to an account already in `config.toml` and replaces its tokens. Use it when tokens were revoked, expired or deleted from the keyring. |
| `rail account list` | Lists configured accounts and whether their tokens are stored, in columns as wide as the longest name and address. It asks the store whether tokens exist without reading them, so a 1Password backend never reveals a token to list accounts. |
| `rail account remove <name-or-address>` | Removes an account's entry in `config.toml` and its stored tokens; its mail stays in the store. Interactive only: it shows what goes (the daemon's text, with the name and address quoted) and asks `Remove? [y/N]`, and only `y` or `yes` removes; if the account changed while you answered, it says so, removes nothing, and asks you to run the command again. When stdin or stdout is not a terminal it removes nothing and exits with status 1; there is no flag to skip the question, so neither a script nor an agent can remove an account for you. |
| `rail daemon status` | Says whether the background daemon is running, with its pid, version, load and socket. Exits 1 when none runs; never starts one. |
| `rail daemon stop` | Asks the daemon to exit once any account being saved is written, and waits (up to 35 seconds) until it has. Says so and exits 1 if it has not exited by then. |
| `rail daemon run` | Runs the daemon in the foreground. Rail starts it this way by itself; you rarely need it. |
| `rail mcp` | Serves Rail's tools to an AI agent over MCP on standard input and output (see [Use Rail from an AI agent](#use-rail-from-an-ai-agent-mcp)). |
| `rail doctor` | Checks the config file, the store, the secrets backend, every account's tokens and `ai_send` policy, and the daemon. The checks run in the daemon, so if it cannot start, `rail doctor` fails outright with the reason (for example a socket path too long for a Unix socket; set `RAIL_RUNTIME_DIR` to a shorter directory). Exits non-zero for config, store, secrets-backend and token-read errors; accounts with no tokens show as `missing` without failing the check (see [Check everything](#check-everything)). |

## Setup

Rail ships no OAuth client of its own. Create one per provider once, then log in, either in the
wizard or with the flags shown here.

### Gmail / Google Workspace

1. In Google Cloud Console create a project and enable the **Gmail API**.
2. OAuth consent screen: External. **Publishing status must be "In production"**, even unverified.
   An app left in "Testing" expires refresh tokens after 7 days and Rail will keep asking you to log in.
3. Credentials: create an **OAuth client ID** of type **Desktop app**. Copy the client id and secret.
   A desktop-app secret is not actually secret; keeping it in `config.toml` is fine.
4. Log in with `rail setup`, or:

       rail account add gmail --address you@gmail.com \
         --client-id XXXX.apps.googleusercontent.com --client-secret GOCSPX-...

   A browser opens for consent. Google shows an "unverified app" warning for your own app; continue.

<!-- guide:google v1 -->
**The same steps with `gcloud`.** `gcloud` can create the project and enable the API, but it cannot
create a desktop OAuth client, so steps 2 and 3 stay in the browser. Pick a project id: 6 to 30
characters, lowercase letters, digits and hyphens, starting with a letter and not ending with a
hyphen:

```sh
gcloud auth login
gcloud projects create <project-id>
gcloud config set project <project-id>
gcloud services enable gmail.googleapis.com
```

An AI agent connected over MCP walks you through exactly these steps (see
[Use Rail from an AI agent](#use-rail-from-an-ai-agent-mcp)). Then, in the browser:

- the consent screen at `https://console.cloud.google.com/apis/credentials/consent?project=<project-id>`:
  External, then publish ("In production");
- the credentials page at `https://console.cloud.google.com/apis/credentials?project=<project-id>`:
  Create credentials, OAuth client ID, Desktop app, and download the JSON file.

### Microsoft 365 / Outlook

1. In Azure Portal, **App registrations** > New. Supported account types: pick what matches your
   mailbox (work/school, personal, or both). Redirect URI: platform **Mobile and desktop applications**,
   value `http://127.0.0.1`. Rail listens on `http://127.0.0.1:<random port>`, and Azure matches any
   port for a loopback redirect. If the portal refuses an `http://127.0.0.1` URI, add it by editing the
   app's **Manifest** (`replyUrlsWithType`) instead.
2. Under **Authentication**, enable "Allow public client flows".
3. API permissions: Microsoft Graph delegated `Mail.ReadWrite`, `Mail.Send`, `User.Read`, `offline_access`.
4. Log in with `rail setup`, or:

       rail account add m365 --address you@corp.com --client-id <application-id>

   The client ID is the **Application (client) ID** on the app registration's **Overview** page
   (not the *Object ID*). If you chose "Accounts in this organizational directory only" (single
   tenant), you must also give the tenant: add `--tenant <tenant-id-or-domain>`, or fill in the
   wizard's *Tenant* field. Microsoft refuses the default `common` endpoint for single-tenant apps;
   add `--tenant` too if your organisation restricts `common`.

<!-- guide:microsoft v1 -->
**The same steps with `az`.** The Azure CLI can do the whole registration of an app for work or
school accounts (single or multiple organisations); personal Microsoft accounts use the manual steps
above. Use the tenant id or a
domain the tenant owns for `<tenant>`, and `AzureADMyOrg` (one organisation) or
`AzureADMultipleOrgs` for `<audience>`:

```sh
az version
az login --tenant <tenant>
az account show --query tenantId --output tsv
az ad app create --display-name Rail --sign-in-audience <audience> --public-client-redirect-uris http://127.0.0.1 --is-fallback-public-client true --query appId --output tsv
az ad sp show --id 00000003-0000-0000-c000-000000000000 --query "oauth2PermissionScopes[?value=='Mail.ReadWrite' || value=='Mail.Send' || value=='User.Read' || value=='offline_access'].{value:value, id:id}" --output table
az ad app permission add --id <app-id> --api 00000003-0000-0000-c000-000000000000 --api-permissions <mail-readwrite-id>=Scope <mail-send-id>=Scope <user-read-id>=Scope <offline-access-id>=Scope
```

`az ad app create` prints the Application (client) ID; use it as `<app-id>` and as `--client-id`.
`az ad sp show` lists the ids of the four Microsoft Graph permissions for `az ad app permission add`.
An administrator can then approve Rail for the whole organisation. Approval needs the app's identity
in the tenant (an "already exists" error from the first command means it is done); if the second
command reports that the permissions or the service principal are not found, wait a minute and run it
again, because directory changes take a moment to propagate:

```sh
az ad sp create --id <app-id>
az ad app permission admin-consent --id <app-id>
```

### Work or school accounts

This section is complete on its own: it covers everything a person or an administrator needs for a
Microsoft 365 work or school mailbox.

**Single-tenant or multi-tenant registration.** When you register the app, "Accounts in this
organizational directory only" (single tenant) is the right choice for one organisation. Log in
through that organisation's tenant: put its tenant ID or domain in the wizard's *Tenant* field, or
pass `--tenant`. Microsoft refuses the default `common` endpoint for single-tenant apps. Choose
"Accounts in any organizational directory" (multi-tenant) only when people from several
organisations will use the same registration; then `common` works, but each organisation still has
to consent.

**Finding the tenant ID.** In Azure Portal open **Microsoft Entra ID** > **Overview** and copy the
*Tenant ID*, or open your app registration's **Overview** and copy the *Directory (tenant) ID*. A
domain the tenant owns (`contoso.onmicrosoft.com`, or a verified domain such as `contoso.com`) works
in the same places.

**Enable public client flows.** Rail is a desktop app and holds no client secret. In the app
registration open **Authentication** and set "Allow public client flows" to **Yes**. Without it
Microsoft answers with `AADSTS7000218`.

**Permissions.** Under **API permissions** add Microsoft Graph *delegated* permissions
`Mail.ReadWrite`, `Mail.Send`, `User.Read` and `offline_access`. Under Microsoft's defaults these
delegated permissions do not require admin consent, but many organisations block user consent for
every app (see below).

**Redirect URI.** Add the platform **Mobile and desktop applications** with the value
`http://127.0.0.1`. Rail uses the same URI for login and for admin consent, and Azure matches any
port on a loopback address.

**Admin consent.** Many organisations stop users from approving apps themselves. An administrator
approves Rail once for everyone, in any of three ways:

- *From the wizard.* On the Details step tick **I am an administrator of this tenant**. Rail opens
  Microsoft's admin-consent page for your tenant, waits for the redirect to `http://127.0.0.1`, and
  confirms. The tenant must be a concrete tenant ID or domain, not `common`; if you left it empty
  or at `common`, the wizard asks for it. Then it continues with your own login.
- *From the portal.* In Azure Portal open the app's **API permissions** and choose **Grant admin
  consent for <organisation>**.
- *During a normal login.* An administrator who just logs in (without ticking the box) is shown
  Microsoft's consent page with a **Consent on behalf of your organization** checkbox. Ticking it
  grants consent for everyone, exactly like the other two ways.

After any of these, users should no longer see the consent prompt for exactly the permissions listed above,
unless the tenant's other policies (Conditional Access, app assignment, MFA) intervene; the table
below lists the `AADSTS` codes for those. If you are not an administrator and your login fails
with `AADSTS65001` or `AADSTS90094`, the wizard shows the exact admin-consent URL. Press `c` to copy
it, send it to an administrator, and press `r` to retry once they have approved.

**When Microsoft refuses.** The error names an `AADSTS` code:

| Code | Meaning | What to do |
|---|---|---|
| `AADSTS65001` | Nobody has consented to Rail's permissions yet. | An administrator grants admin consent (wizard step or portal). Non-administrators send them the URL the wizard shows. |
| `AADSTS90094` | Your organisation requires an administrator to approve apps. | Same as above: send the admin-consent URL to an administrator, then retry with `r`. |
| `AADSTS50076`, `AADSTS50079` | Multi-factor authentication or another policy step is required. | Complete the prompt Microsoft shows in the browser. If it keeps failing, ask your administrator which policy applies; Rail cannot bypass it. |
| `AADSTS50194` | A single-tenant app was used with the `common` endpoint. | Set the tenant to your tenant ID or domain. |
| `AADSTS50020` | The account belongs to a different tenant than the app. | Log in with an account from the app's tenant, or register the app for multiple organisations. |
| `AADSTS700016` | The application is not found in that tenant. | Check the client ID (the *Application (client) ID*, not the object ID) and the tenant. |
| `AADSTS7000218` | Microsoft expected a client secret. | Enable "Allow public client flows" under **Authentication**. |
| `AADSTS50011` | The redirect URI does not match the registration. | Add `http://127.0.0.1` under **Mobile and desktop applications**. |
| `AADSTS53003` | A Conditional Access policy blocked the login. | Ask your administrator; the policy, not Rail, decides. |

**Tokens that expire early.** Some organisations enforce a sign-in frequency policy that ends a
session after hours or days. When that happens a token refresh fails with "refresh token rejected" and
names `rail account login <name>` as the fix. Log in again; this replaces the tokens and leaves
`config.toml` alone:

    rail account login work

See [Logging in again](#logging-in-again).

### Presets for your organisation

An administrator can save every colleague the portal work: register the app once, then hand out a
preset file that fills in the wizard. A preset never contains tokens, and Rail never writes preset
files.

1. **Register the app once**, as in [Microsoft 365 / Outlook](#microsoft-365--outlook) and
   [Work or school accounts](#work-or-school-accounts): single tenant is usually right, redirect
   URI `http://127.0.0.1` under **Mobile and desktop applications**, delegated Graph permissions
   `Mail.ReadWrite`, `Mail.Send`, `User.Read`, `offline_access`.
2. **Enable public client flows** under **Authentication**.
3. **Grant admin consent** for the organisation, in the portal (**API permissions** > **Grant admin
   consent**) or with the wizard's administrator box.
4. **Write the preset file**:

   ```toml
   name = "Contoso Microsoft 365"       # shown in the wizard's preset list
   kind = "m365"                        # or "gmail"
   client_id = "00000000-0000-0000-0000-000000000000"
   tenant = "contoso.onmicrosoft.com"   # Microsoft only
   # client_secret = "GOCSPX-..."       # optional; Gmail desktop apps need it
   # help_url = "https://intranet.example/rail"   # optional "Need help?" link (F1 in the wizard)
   ```

   `kind` and `client_id` are required; `name`, `tenant`, `client_secret` and `help_url` are
   optional. Unknown keys and wrong types are rejected with the file's path and line number, like
   `config.toml`. A preset fills in the app registration; users still enter their own address.
5. **Publish it**, in one of two ways:
   - on the intranet over `https`, so people can type the URL;
   - as a file under `/etc/rail/presets/*.toml` on each machine, or in a user's
     `~/.config/rail/presets/*.toml` (`$RAIL_CONFIG_DIR/presets` when that is set).

   The wizard lists the presets it finds in those two directories. A user can also type a preset's
   path or an `https://` URL on the preset step. Rail fetches a URL with the same timeouts as its
   logins, refuses plain `http://` (anyone on the network could alter the file), refuses redirects
   (an intranet URL behind single sign-on fails with "HTTP 302"; use the file instead), refuses
   bodies over 64 KiB, and never saves the fetched file.

For Gmail, register a Desktop app client as in [Gmail / Google Workspace](#gmail--google-workspace)
and include its `client_secret`.

**What a colleague then does:**

1. Run `rail`. With no config the setup wizard opens (`rail setup` does the same at any time).
2. Choose **Use a preset from my organisation** and pick the preset, or type its path or URL.
3. Enter their email address.
4. Log in in the browser. The wizard reports where everything was saved.

### Logging in again

If a provider revokes Rail or a sign-in frequency policy ends the session, a token refresh fails and
the error names the fix. If the tokens are deleted from the keyring, `rail doctor` reports them as
missing and names the same fix:

    rail account login work

The argument is the account's name or its address, matched without regard to case. Rail runs the
browser login for the account already in `config.toml` and replaces its tokens; the config file is
not touched.

### Check everything

    rail doctor
    rail account list

`rail doctor` reports the config file and how many accounts it holds, the store and its schema
version, the secrets backend, and token presence for every account, then ends with an `ok   daemon`
line naming the daemon that ran the checks. Text Rail did not write (a keyring's or SQLite's
message, the config parser's) is marked `from outside Rail` and shown in quotes with control
characters escaped. It exits non-zero for config, store,
secrets-backend and token-read errors. An account with no stored tokens is listed as `missing`, with
the `rail account login` advice, but does not make the exit status non-zero, so scripts that care
should grep for `missing`.

## Configuration

The wizard and `rail account add` write `config.toml` for you and preserve any comments you add later.
A minimal file looks like this:

```toml
[general]
# "keyring" (default) stores tokens in the OS keyring; "file" uses a private file instead.
secrets = "keyring"

[[accounts]]
name = "work"
kind = "gmail"            # "gmail" or "m365"
address = "you@example.com"
client_id = "XXXX.apps.googleusercontent.com"
client_secret = "GOCSPX-..."   # Gmail only
color = "blue"
```

Tokens are never written to `config.toml`.

### What an AI agent may do with your mail: `ai_send`

Each account may set what a coding agent connected over MCP may do with its mail:

```toml
[[accounts]]
name = "work"
# ...
ai_send = "confirm"   # "confirm" (the default), "drafts", or "client"
```

- `confirm`: Rail shows you, through your MCP client's confirmation dialog (MCP elicitation), the
  recipients, the subject and the start of the body, and sends only on an explicit yes. A client
  without that dialog will get drafts-only behaviour; that fallback arrives with the send
  tools.
- `drafts`: the agent may create and edit drafts, never send.
- `client`: Rail relies on your MCP client's own per-call approval prompt. This is the weakest
  option.

The policy will cover sending, replying, forwarding and deleting. Mail tools do not exist yet;
today the key is only recorded and reported (`rail doctor`, `status`, `accounts.list`), and
nothing enforces it.

### Where things live

| What | Linux path |
|---|---|
| Config | `~/.config/rail/config.toml` |
| Presets | `~/.config/rail/presets/*.toml`, `/etc/rail/presets/*.toml` |
| Store | `~/.local/share/rail/rail.db` |
| Tokens | OS keyring (service `rail`), `~/.local/share/rail/secrets.json` with `secrets = "file"`, or 1Password items with `secrets = "1password"` |
| Logs | `~/.local/state/rail/rail.log` |
| Daemon socket | `$XDG_RUNTIME_DIR/rail/rail.sock` (the state directory where there is no runtime directory, as on macOS) |

macOS uses the platform directories under `~/Library/Application Support/rail` instead. Override any
location with `RAIL_CONFIG_DIR`, `RAIL_DATA_DIR`, `RAIL_CACHE_DIR`, `RAIL_STATE_DIR` and
`RAIL_RUNTIME_DIR` (the directory that holds the daemon's socket).

No Secret Service on your box (tty-only Linux)? Put this in `config.toml` before adding accounts:

```toml
[general]
secrets = "file"
```

Files that hold mail or credentials are created owner-only and written atomically.

### The background daemon

The setup wizard, the `rail account` commands and `rail doctor` talk to one background process,
the Rail daemon, which runs every operation, so two of them running at once never race on the
store, the config file or a login. The wizard starts (or reaches) the daemon before it takes over
the terminal, so a daemon that cannot start is reported as a plain error. The daemon opens the
login and consent pages, receives the tokens and saves the account; tokens, client secrets and
authorization codes never pass through the wizard. A path you type in the wizard (a preset file)
is resolved against the directory you ran `rail setup` in before it is sent. If the daemon stops
while the wizard is open (`rail daemon stop`, or a crash), the wizard's next request connects to a
fresh daemon, starting one if needed. A request that only reads is sent again on the new connection;
one that changes something (beginning a login or a consent) is not, and the wizard asks you to try
again. A login that was waiting in the stopped daemon is gone: retry it.
You never start it yourself: the first command that needs it starts `rail daemon run` in the
background, and it exits after ten minutes with no client connected and no login waiting. Its
logs go to `rail.log`; it never writes to the terminal.

The daemon listens on a Unix socket (see [Where things live](#where-things-live)) in a directory
only you can open (mode 0700); the socket itself is 0600, and the daemon drops any connection
from another user. `rail daemon status` shows what it is doing and `rail daemon stop` stops it.
A login you begin in a command keeps waiting in the daemon until you finish it in the browser,
cancel it with Ctrl-C, or it times out after 15 minutes; once the account is being saved, the
save finishes even if the command that started it is gone. `rail daemon run` in the foreground
stops the same way on Ctrl-C or SIGTERM: saves in flight finish first, then it exits 0.

The daemon starts with the environment of the command that started it: `DISPLAY` and
`WAYLAND_DISPLAY` (where the browser opens), `BROWSER`, `OP_SESSION_*` (a signed-in 1Password
CLI) and `PATH`. It keeps that environment until it exits, ten idle minutes later; run
`rail daemon stop` to have the next command start one with your current environment.

It also works on the directories of the command that started it, made absolute (a relative
`RAIL_CONFIG_DIR=cfg` means `cfg` under that command's working directory). A later command whose
`RAIL_CONFIG_DIR`, `RAIL_DATA_DIR` or `RAIL_STATE_DIR` names other directories (compared after
resolving symlinks, so a symlinked config directory still counts as the same), but shares the
same runtime directory, is refused with "a Rail daemon is already running with different
directories" and changes nothing (as is one too old to say which directories it uses, unless
its version differs, in which case it is replaced as usual); stop that daemon with `rail daemon stop` from the shell that
started it, or use the same `RAIL_*` settings. Give each setup its own `RAIL_RUNTIME_DIR` to run
two side by side.

When a command or a step of the setup wizard fails, it shows Rail's own message and, on a line
starting `hint:`, what to do next. Words from outside Rail (a provider's error description, the system's error text) come on a
line of their own, quoted and with control characters escaped. No error ever shows a token, a
client secret or an authorization code.

### Tokens in 1Password

With the [1Password CLI](https://developer.1password.com/docs/cli/) (`op`, version 2) installed and
signed in, Rail can keep tokens in a vault instead of the OS keyring:

```toml
[general]
secrets = "1password"

[onepassword]
vault = "Rail"                    # required
# account = "my.1password.com"    # when several 1Password accounts are signed in
# op_path = "/usr/bin/op"         # default: op on PATH
```

Rail keeps one item per mail account in that vault, titled `Rail: <address>` and tagged `rail`,
with the access and refresh tokens in concealed fields. Values reach `op` only through a JSON
template on its standard input, never as command-line arguments (which other processes can read).
Each `op` call may take up to 30 seconds, long enough for the 1Password app's unlock prompt. When
1Password is locked or `op` is not signed in, Rail says so and asks you to unlock the app or run
`op signin`; `rail doctor` checks this with `op whoami`. Rail resolves `op_path` once (following symlinks) and refuses it when it is
relative, not an executable file, owned by another user, or writable by anyone but its owner, or when
any directory above it belongs to another user or is writable by others (a sticky directory such as
`/tmp` is fine); the resolved file is what runs. `op_path`
is only ever read from `config.toml`; no daemon request can set it. A Homebrew-on-Linux `op` usually
fails this check, because its prefix (`/home/linuxbrew/.linuxbrew`) is writable by a group or owned by
another user; Rail then says which part it refused. Prefer 1Password's own package (for
example `/usr/bin/op`). If you instead copy a binary into a directory only you can write, use
`install -m 0755`, and set `op_path` to it by hand. A copy loses the package's setgid group, which
can break the desktop-app integration (`op` then asks you to sign in separately). The 1Password
Homebrew cask is macOS-only, so this mainly concerns manual and Linuxbrew installs. The daemon's `secrets.configure`
operation switches backends, but only for a request a front end marked `confirm` after asking the
user, carrying the digest of the plan the user was shown: tokens are copied to the new backend, read back to verify, and only
then are the old copies deleted, so a failure part way leaves the old copies in place.

A Gmail `client_secret` can live in 1Password too. In `config.toml`, or in
`rail account add --client-secret`, give an `op://vault/item/field` secret reference instead of the
value (an agent can also set `store_secret_in_1password` on `rail_account_add`, which makes Rail store
the secret it was given by file in 1Password and write an `op://` reference into `config.toml`; it
needs a `[onepassword]` vault in `config.toml` and a client secret to store, and the CLI and the
wizard never do this):

```toml
client_secret = "op://Rail/Google client/client_secret"
```

Rail reads it with `op read` only when a login needs it, and never caches, logs or writes the
value. This works with any token backend.

To move existing tokens from the keyring or the file into 1Password (or back), ask an agent to call
`rail_secrets_configure` with `backend = "1password"` and your vault, and confirm the change in its
dialog. Rail copies each account's tokens, reads them back from the new backend, writes
`config.toml`, and only then deletes the old copies; a failure part way leaves the old copies and
the old config in charge. `op_path` is never set this way: only you choose, by editing
`config.toml`, which program Rail runs as `op`.

## Project layout

```
crates/rail-core/        Library: paths, config, store, auth, onboarding, operations, daemon. No terminal dependency.
  src/config/            TOML model, loading with line-numbered errors, comment-preserving edits
  src/store/             SQLite open, migrations, accounts repository
  src/auth/              Secret stores (keyring, file, 1Password), OAuth endpoints, loopback listener, login and admin-consent flows, token refresh
  src/onboarding/        Presets and the setup wizard's pure state machine
  src/ops/               The operation layer: every request and response type, OpError, Untrusted
  src/daemon/            JSON-RPC wire format, client and connector, the socket server and its handlers
  src/accounts/          The add, login and remove pipeline the daemon runs
  src/guide/             Google and Microsoft setup steps as versioned data
  src/doctor.rs          The `rail doctor` checks
crates/rail/             Binary: clap CLI, logging, `account`, `daemon`, `doctor`, `mcp` and `setup` commands
  src/cmd/mcp/           The MCP server: tools, the setup prompt, confirmation through elicitation, summaries
  src/tui/               App shell: terminal guard, event loop, widgets, the wizard screen and its effects
  tests/cli.rs           End-to-end tests against the built binary in a temporary XDG layout
  tests/cli/daemon.rs    The daemon through the binary: on-demand start, status, stop, directories, signals
  tests/cli/pty.rs       The same in a pseudo-terminal: the wizard's Ctrl-C and `rail account remove`'s question
  tests/mcp.rs           `rail mcp` driven by an MCP client that accepts, declines or cannot show confirmations
  tests/mcp/guarded.rs   The same for the two confirmed tools, removal and secret-store changes
assets/brand/            Logos for light and dark backgrounds, and the app icon
docs/superpowers/        Design specs and phase plans
deny.toml                Licence and advisory policy for `cargo deny`
```

## Development

    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --all --check
    cargo deny check

Line coverage (needs `cargo-llvm-cov`):

    cargo llvm-cov --workspace

Tests never touch the real XDG directories, the real OS keyring, the network, a real browser or a
real 1Password account (a fake `op` script stands in).
Debug builds honour three test hooks, `RAIL_TEST_OPENER` (a program run instead of the browser),
`RAIL_TEST_TOKEN_URL` (a token endpoint served by the test) and `RAIL_TEST_IDLE_SECS` (the daemon's
idle timeout, so the daemons the binary tests start exit seconds after their last client); release
builds ignore all three. Every binary test runs with the stand-in browser set, so neither it nor a
daemon it starts can open a real one. On Linux, the tests that drive the wizard and `rail account remove` in a
pseudo-terminal use util-linux `script` and are skipped where `script` is missing. The tests that
exercise the real keyring and a real 1Password vault are ignored by default:

    cargo test -p rail-core -- --ignored keyring_live
    RAIL_OP_LIVE_VAULT=<vault> cargo test -p rail-core -- --ignored onepassword_live

Changes a user can see or type update this README in the same change; see
[CONTRIBUTING.md](CONTRIBUTING.md).

See [CONTRIBUTING.md](CONTRIBUTING.md) for the ground rules and commit format, and
[CHANGELOG.md](CHANGELOG.md) for what has shipped.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
