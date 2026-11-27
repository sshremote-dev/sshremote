# Teams

A team server lets an organization manage its computers and the people who reach them: who may
open which computers and with what rights, the settings every computer keeps, address books shared
with groups and people, each person's own address book on all their devices, and a record of every
session. It is part of the program (`sshremote team serve`)
and runs on the organization's own server, behind its own HTTPS; nothing goes to sshremote.dev.
People sign in to it with a password and an authenticator code, or through the organization's
identity provider (OpenID Connect) or directory (LDAP). Scripts manage it with API tokens. Anyone
not in a team uses the program as before, with no account.

## How access works

A computer **joins** the team with an enrollment token from the console (or the team's own
installer, which joins by itself). Joining gives the computer and the team server a shared secret,
the computer's **team key**, which only they hold. It is kept with the computer's remote access
settings (machine-wide on Windows, where only administrators change them), so joining or leaving
takes the same rights as changing remote access, and the service that runs remote access reads
the team's policy from the server every minute and sends it the audit log's events.

A person signed in to the team asks the server for a **ticket** to one computer. The server
checks the access rules, and returns the ticket: what the person may do there, for five minutes,
bound to the device key of the person's own computer, with a tag made with that computer's team
key. The viewer connects by ID as always (through the relay or straight) and uses the ticket in
place of a password: the password exchange (SPAKE2) runs with the tag, which only the team server
and that computer can make, so the relay and anyone else see nothing they can use, and a ticket
taken from one person's computer does not work from another (the device key is proved in the
same handshake). No password of the computer is given or kept.

## Running a server

```sh
sshremote team serve --listen 127.0.0.1:8023 --data /var/lib/sshremote-team --url https://team.example.com
sshremote team admin NAME            # makes an administrator (asks for a password), on the server
sshremote team admin NAME --totp     # the same, with a new authenticator and its backup codes
```

Put it behind HTTPS (nginx, Caddy) at the `--url` address. The first administrator is made on
the server with `team admin`; the rest is done in the console at that address.

## The console

Devices (joined computers: name, ID, group, owner, note, last seen, version; rename, move to a
group, give an owner and a note, disable, remove; filter by group or owner), users (add, disable,
reset a password, require an authenticator, make new backup codes, administrator or not, groups),
user groups, device groups, access rules (which users or user groups may reach which device
groups, with what rights, and whether the person at the screen is asked first), policies (the
settings a device group keeps: whether a password lets anyone in at all besides team members,
asking, rights, recording, idle end, straight connections, LAN), enrollment tokens (with a group,
an expiry and a number of uses, and the installer lines for Windows, macOS and Linux that join with
them), shared address books (below), API tokens (for scripts: a name, a scope and an expiry; shown
once, revoked at a click), the audit log (connections, ends, refusals, file transfers, sign-ins and
wrong ones, lockouts, and every change made in the console, with who made it; filter and download
as CSV), and settings
(team name and logo shown in the app, the relay its computers use, OpenID Connect, LDAP, email,
and how many days the audit log keeps).

A disabled device stays in the list with its note and owner, but nobody gets a ticket to it, the
people's lists leave it out, and its own requests (its look for the policy, its events, leaving,
joining again) get `403 {"error": "this computer is disabled in the team"}` until it is turned
back on. Sessions open when it is disabled go on: end them first.

Two-step sign-in: setting up an authenticator for someone (or `team admin NAME --totp`) also makes
8 backup codes of 10 letters and digits ("abcde-fghij"), shown once with the QR code and kept only
as hashes. Each works once in place of the authenticator's code (in any case, with or without its
dash), for a lost phone; the users list says how many are left. An administrator makes a new set
for someone, and anyone makes their own on their account page with their password; a new set
ends the old one, and resetting the authenticator takes them away.

Shared address books (Address books): lists of computers an administrator makes, names (up to 64
characters, each name once), describes, and shares with user groups and people, each either to
**view** (they see its computers in the app and open them) or to **edit** (they also add, change
and remove its computers). An administrator makes, renames, shares and removes books there, and
opens one (`#books/ID`) to add, change, remove and find its computers. A computer of a book has a
name (up to 64 characters), an address, up to 10 tags (each up to 32 characters, separated by
commas) and a note (up to 500 characters). The address is a computer's ID (`123 456 789`; with
`@RELAY[:PORT]` after it, `123456789@relay.example.com:21116`, it goes through that relay) or an SSH
address (`user@host`, `user@host:2222`, `host`, `[v6]:22`); a user of 6 to 12 digits before the
`@` is read as an ID. No password is kept in a book, and none can be put in an address. Members
who are no administrators see Address books too: the books shared with them, and in the books
they may edit, the same ways to add, change and remove their computers. A team keeps at most 100
books of at most 2000 computers each. Whom a book is shared with, and what it lets them do, is
looked at again at every request with the person's groups as they are then: someone taken out of
a group, or removed, no longer reaches its books; a user renamed keeps theirs. Each book is kept
in a file of its own in the data folder (`shared-books/ID.json`), written whole at each change.

The audit log keeps 365 days unless the settings say otherwise (0: for ever); once a day the
server drops the older lines, writing `audit.jsonl` whole beside it and putting it in its place.

## In the app

Settings > Team: join this computer to a team (address and token), or leave it; sign in as a
person (the sign-in opens in the browser, so the organization's single sign-on works), or sign
out. Signed in, the home window lists the team's computers the person may reach; opening one gets
a ticket and connects. Under them, each address book the team shares with the person, under its
name: its computers in the same rows, found by the home window's search too (by name, address and
tags), and opened as their address typed in the field would be (an ID through the relay as usual,
or the one the entry names, asking for its password; an SSH address over SSH). They are asked for
with the team's computers, every minute and with the list's refresh button; changing them is done
in the console. With an address book passphrase set, the person's own list of computers is
kept on the team server too, encrypted on the device with a key from that passphrase (the server
stores what it cannot read), and shows on every device they sign in on.

## Protocol (version 1)

All requests and answers are JSON over HTTPS. A person's requests carry
`Authorization: Bearer TOKEN` (from signing in). A device's requests are signed with its team key:
headers `X-Team-Device: ID`, `X-Team-Time: UNIX_SECONDS.RANDOM_HEX`, `X-Team-Sign: HEX` where the
signature is `HMAC-SHA256(team key, "sshremote team request\n" + METHOD + "\n" + PATH + "\n" + TIME + "\n" + BODY)`
(TIME as sent, with its random part); the server refuses a time more than 300 s off and a
signature seen in the last 600 s.

Errors: HTTP 4xx/5xx with `{"error": "words"}`.

### People

- `POST /api/login` `{"name", "password", "code"?}` -> `{"token", "user": USER, "expires"}`.
  A user with an authenticator gets `401 {"error": "code needed", "code": true}` when the password
  is right and no code came (so the answer tells nothing of who has an account to someone without
  its password); a code works once, and so does a backup code in its place. Tried against LDAP
  when it is set up and the user is not local. Five wrong tries from one address (an IPv6 one by its /64), or twenty at one account,
  lock them out for 15 minutes. Behind a proxy on the same computer the client's address is the
  last of `X-Forwarded-For` (nginx's `$proxy_add_x_forwarded_for`, Caddy's default).
- `GET /api/login-options` -> `{"oidc": bool, "ldap": bool, "team": TEAM}` (no sign-in needed).
- The app signs in in the browser (RFC 8252): it waits on a loopback port of its own computer,
  keeps a random verifier, and opens `GET /login?app=CHALLENGE&port=PORT` where CHALLENGE is the
  hex SHA-256 of the verifier. Signed in there (`POST /api/login` with `"app"` and `"port"`, which
  answers with `"to"`; or single sign-on), the browser goes on to
  `http://127.0.0.1:PORT/?code=CODE`, and the app takes its session with
  `POST /api/app-login {"code", "verifier"}` -> `{"token", "user", "expires"}` (a code is used
  once, right or wrong, and kept 2 minutes). A sign-in link someone else made and sent gives them
  nothing: the code goes only to the computer whose browser signed in, and works only with the
  verifier the app that started it holds.
- `GET /api/oidc/start?app=CHALLENGE&port=PORT` (both left out for the console) -> redirect to
  the provider (code flow with PKCE and state, with a cookie that ties it to this browser);
  `GET /api/oidc/callback?code&state` -> signs in, then on to the console with `#session=TOKEN`
  (which the console takes only in the tab that began it) or, for the app, to its sign-in page
  with `#handoff=CODE`, where one click goes on to the app's loopback address (`#error=WORDS` on
  the sign-in page when it failed).
- `POST /api/logout` ends the session.
- `POST /api/reset` `{"name"}` (a name or an email address) -> `{"sent": true}` whatever the case:
  when the team has a mail server and that account a password and an email address, a link to set
  a new password goes to it (an hour; three asks per account and the usual tries per address in
  15 minutes). `GET /api/link?token=T` -> `{"user", "reset"}` while a link works;
  `POST /api/link` `{"token", "password"}` sets the password, uses the link up and ends the
  person's sessions. The console opens links sent as `URL/#link=TOKEN`.
- `GET /api/me` -> `{"user": USER, "team": TEAM}`.
- `GET /api/me/backup-codes` -> `{"left": N, "totp": bool}`; `POST /api/me/backup-codes`
  `{"password"}` -> `{"backup_codes": [8 codes]}`, a new set shown once (the old ones end). The
  password is the account's own, or its directory's; a wrong one is `403` and counts as a wrong
  try from the address.
- `GET /api/devices` -> `{"devices": [{"id", "name", "os", "group", "online": bool, "rights", "ask"}]}`:
  the devices the person may reach (online: seen in the last 3 minutes).
- `POST /api/ticket` `{"device": ID, "key": HEX32, "viewer": ID?}` -> `{"ticket": B64URL, "tag": HEX, "expires"}`
  where `key` is the viewer's device key (X25519, public) and `viewer` its own remote-access ID if
  it has one. 403 when no rule lets the person reach it.
- `GET /api/book` -> `{"book": B64 or null, "salt": HEX}`; `PUT /api/book` `{"book": B64}`: the
  person's own address book, sealed on the device (ChaCha20-Poly1305 with a key from PBKDF2 of
  their passphrase and `salt`); at most 1 MB.
- `GET /api/books` -> `{"books": [{"id", "name", "description", "role": "view"|"edit", "entries": [ENTRY]}]}`:
  the shared address books the person is in (by name, or through a user group), by name, with
  what they may do in each (shared with them in two ways, the one that lets more; an administrator
  may edit any) and their computers; whom else a book is shared with is not said.
- `POST /api/books/ID/entries` `{"name", "address", "tags", "note"}` -> `{"entry": ENTRY}`;
  `PUT /api/books/ID/entries/EID` (each field left as it is when not sent) -> `{"entry": ENTRY}`;
  `DELETE /api/books/ID/entries/EID` -> `{}`: for those the book lets edit, and administrators.
  A book not shared with the person is `404` as one that is not there; one they may only view,
  `403`. `tags` is a list or words with commas between them.

ENTRY = `{"id", "name", "address", "tags": [words], "note"}` (no password: addresses are an ID, as
`123 456 789` or `123456789@relay.example.com:21116`, or an SSH address, as `user@host:2222`).
USER = `{"name", "display", "email", "admin": bool, "groups": [names], "totp": bool}`.
TEAM = `{"name", "logo": DATA_URL or "", "relay": "HOST[:PORT]" or "", "url"}`.

### Devices

- `POST /api/device/enroll` `{"token", "id", "name", "os", "version"}` -> `{"key": HEX32, "team": TEAM}`.
  Unsigned (the token is the proof); an ID already joined is joined again with a new key.
- `GET /api/device/policy` (signed) -> `{"policy": POLICY, "team": TEAM, "end": true?}` (end: the
  console asked to end its sessions); also
  marks the device seen, with its version from `X-Team-Version`.
- `POST /api/device/events` (signed) `{"events": [EVENT]}` -> `{}` (kinds and ways it may say of
  itself only, as below).
- `POST /api/device/leave` (signed) -> `{}`.

POLICY = `{"password": bool, "ask": bool|null, "rights": u8, "record": bool|null, "idle": minutes|null,
"direct": bool|null, "lan": bool|null, "incoming_only": bool}`: `password` false lets in team
tickets only; `rights` caps what anyone may do (bits as in `src/grant.rs`); null leaves the
computer's own setting; `incoming_only` shows this computer alone in its app (its ID, for the
people who are helped, with no computers of their own to open). The app shows what a policy sets
as set by the team, and does not let it be changed there.

EVENT = `{"time", "kind": "in"|"out"|"refused"|"file", "how": "team"|"password"|"one-time"|"request",
"user"?, "from"?, "rights"?, "secs"?, "words"?}`; a file event's words say what was done in English
("Uploaded PATH (SIZE)", "Downloaded PATH", "Made the folder PATH", "Renamed PATH to NAME",
"Deleted PATH"), which the console and the app translate. The server adds its own kinds:
`login`, `ticket`, `join`, `leave`, `removed`, `invite`, `reset`, `password`, and:

- `login-failed`: a wrong sign-in, with `user` (the account's name, or the name as typed when
  there is none), `from` and `words` saying why ("wrong password", "wrong two-step code", "no such
  user", "the directory refused the name or password", "this account is disabled", ...); `locked`:
  a sign-in turned away for too many wrong tries (`429`). Each at most once a minute for one
  address and name, so a flood of tries does not fill the log.
- `admin`: a change made in the console or by an API token: `user` is who made it (for a token,
  the administrator who made the token, with `"how": "token"` and the token's `token` name),
  `from`, and `words` saying what in English ("user bo changed", "user bo renamed to bob", "new
  password for user bo", "device group office policy changed", "rule list changed", "settings
  changed", "enrollment token made", "device Desk disabled", ...), which the console translates;
  never a secret. A change of a device names it in `device` and `device_name`.
- `token`: an API token made ("API token deploy made (administration)") or revoked.

### Administration (an administrator's session, or an API token)

- `GET|POST /api/admin/users`, `PUT|DELETE /api/admin/users/NAME`
  (`{"name", "display", "email", "admin", "groups", "password"?, "password_clear"?, "disabled", "totp"?, "totp_reset"?}`;
  `"totp": true` makes an authenticator and answers with `totp_secret`, `totp_uri`, `totp_qr` and
  `backup_codes` once; `totp_reset` takes the backup codes away too). The list has
  `"backup_codes": N`, how many are left. `POST /api/admin/users/NAME/backup-codes` ->
  `{"backup_codes": [..], "user"}`: a new set for someone with an authenticator. Single sign-on
  finds a person by the issuer and subject it linked, else by an email the provider says it
  verified, in an allowed domain, and links it only to an account with no password,
  authenticator or administrator's rights.
- `GET|POST /api/admin/groups`, `DELETE /api/admin/groups/NAME`; the same for `/api/admin/device-groups`
  (a device group has a `policy`).
- `GET /api/admin/devices`, `PUT|DELETE /api/admin/devices/ID` (`{"name", "group", "owner", "note",
  "disabled"}`, each left as it is when not sent: `owner` a user's name or "", `note` at most 500
  characters; `{"end": true}` ends its sessions at its next look). A device is listed with these
  too.
- `GET|PUT /api/admin/rules` (`[{"who": "user:NAME"|"group:NAME"|"all", "devices": "group:NAME"|"device:ID"|"all", "rights": u8, "ask": bool}]`).
- `GET|POST /api/admin/tokens`, `DELETE /api/admin/tokens/ID` (`{"group", "expires", "uses"}` -> with `"token"`).
- `GET /api/admin/audit?from&to&device&user&kind&limit` -> `{"events": [EVENT + "device"]}`;
  `GET /api/admin/audit.csv` the same as CSV (columns time, device, device_name, kind, how, user,
  from, rights, secs, words, token; every cell quoted, none a spreadsheet would run as a formula).
- `GET|POST /api/admin/api-tokens`, `DELETE /api/admin/api-tokens/ID`: API tokens (below).
- `GET|POST /api/admin/books`, `GET|PUT|DELETE /api/admin/books/ID`: shared address books.
  BOOK = `{"id", "name", "description", "members": [{"who": "user:NAME"|"group:NAME", "role": "view"|"edit"}], "count", "created"}`
  (`count`: how many computers it has); the list is `{"books": [BOOK]}`, one book
  `{"book": BOOK + "entries": [ENTRY]}`, and so is the answer to a change. `POST` and `PUT` take
  `{"name", "description", "members", "entries"?}`, each left as it is when not sent; `entries`, when
  sent, takes the place of all of a book's computers (a script keeping a book as another list has
  it; one that gives the `id` of one there keeps it). Names of a user or group there is, each once;
  at most 100 books, 2000 entries a book, 1000 members, 500 characters of description; a request
  here is at most 2 MB. Each change is an `admin` event ("address book Servers added", "sharing of
  address book Servers changed", "entry Build added to address book Servers", "entries of address
  book Servers replaced", ...), an editor's under their own name.
- `GET|PUT /api/admin/settings` (`{"name", "logo", "relay", "audit_days": 0..36500, "oidc": {"issuer", "client_id",
  "client_secret", "domains": [..], "make_users": bool, "trust_email": bool} | null, "ldap": {"url",
  "user_dn": "uid={},ou=people,dc=example,dc=com", "make_users": bool, "ca_file": PATH} | null,
  "smtp": {"url": "smtps://HOST[:PORT]" | "smtp://HOST:587", "user", "password", "from"} | null}`;
  secrets are never sent back, only `client_secret_set` and `password_set`). A directory sign-in
  binds as the person and reads their own entry for the name and email of an account it makes
  (`ca_file`: the authority of a directory certificate the system does not know); `smtp://` uses
  STARTTLS except to a mail server on the same computer.
- `POST /api/admin/invite` `{"name"}`: an email to that person with a link to set their password
  (a week). `POST /api/admin/mail-test` `{"to"?}`: a test email (to the administrator's own address
  when none is given).

### API tokens

A script manages the team with a token an administrator made in the console (API tokens), sent as
`Authorization: Bearer TOKEN` to `/api/admin/...` as a session is.

- `POST /api/admin/api-tokens` `{"name", "scope": "read"|"devices"|"admin", "expires": TIME|0}` ->
  the token as listed, with `"token": "sshrt_..."` shown this once (kept only as its hash);
  `GET /api/admin/api-tokens` -> `{"tokens": [{"id", "name", "scope", "by", "created", "used",
  "expires"}]}` (`used`: when it was last used, to the minute; 0: never; `expires` 0: never);
  `DELETE /api/admin/api-tokens/ID` revokes it. At most 100, each name once.
- `read` may `GET` the administration (the lists, the audit log and its CSV, the settings without
  their secrets); `devices` only `/api/admin/devices` and `/api/admin/devices/ID`; `admin` what an
  administrator may, but never what signs a person in: no password set or taken away, no
  authenticator set up or reset, no new email address for an account with a password (a link to
  set a new one would go there), no invitation, no backup codes, and no API tokens (only an
  administrator signed in manages those). Otherwise `403`.
- A token acts for the administrator who made it, while they are one: taken their rights (or
  disabled), it gets `403`; removed, their tokens go with them. Expired or revoked, `401`.
- A token is no session: `/api/login`, `/api/me`, `/api/devices`, `/api/ticket`, `/api/book`,
  `/api/books` (and a book's entries there) and `/api/logout` do not take it. A script changes a
  shared book's computers with `PUT /api/admin/books/ID` and its `entries`: `read` may only get the
  books, `admin` change them, `devices` neither.

### Installers

`GET /install.sh?token=T` and `GET /install.ps1?token=T`: the official one-line installers from
sshremote.dev, then `sshremote team join URL T` and remote access turned on with the service: a
computer that runs one is in the team. For many Windows computers, the MSI does the same silently
(Group Policy, Intune): `msiexec /i sshremote-windows-x86_64.msi /qn SERVICE=1 TEAM_URL=URL
TEAM_TOKEN=T`.

### Ticket

`ticket` is base64url of compact JSON `{"v": 1, "d": DEVICE_ID, "u": NAME, "n": DISPLAY,
"r": RIGHTS, "a": ASK, "k": VIEWER_KEY_HEX, "i": VIEWER_ID, "e": EXPIRES, "x": NONCE_HEX}` and
`tag = HMAC-SHA256(team key of DEVICE_ID, "sshremote team ticket\n" + ticket)`. The viewer sends
`ticket` in the handshake (flag `TEAM`) and runs the password exchange with the tag as the
password; the computer computes the tag itself, refuses an expired ticket or one for another
ID, then asks for the viewer's device key proof (the `WHO` step) and lets it in only with the
key the ticket names.
