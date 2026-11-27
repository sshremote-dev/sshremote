# Teams

A team server lets an organization manage its computers and the people who reach them: who may
open which computers and with what rights, the settings every computer keeps, one address book
for everyone, and a record of every session. It is part of the program (`sshremote team serve`)
and runs on the organization's own server, behind its own HTTPS; nothing goes to sshremote.dev.
People sign in to it with a password and an authenticator code, or through the organization's
identity provider (OpenID Connect) or directory (LDAP). Anyone not in a team uses the program as
before, with no account.

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
```

Put it behind HTTPS (nginx, Caddy) at the `--url` address. The first administrator is made on
the server with `team admin`; the rest is done in the console at that address.

## The console

Devices (joined computers: name, ID, group, last seen, version; rename, move to a group, remove),
users (add, disable, reset a password, require an authenticator, administrator or not, groups),
user groups, device groups, access rules (which users or user groups may reach which device
groups, with what rights, and whether the person at the screen is asked first), policies (the
settings a device group keeps: whether a password lets anyone in at all besides team members,
asking, rights, recording, idle end, straight connections, LAN), enrollment tokens (with a group,
an expiry and a number of uses, and the installer lines for Windows, macOS and Linux that join with
them), the audit log (connections, ends, refusals, file transfers; filter and download as CSV),
and settings (team name and logo shown in the app, the relay its computers use, OpenID Connect,
LDAP).

## In the app

Settings > Team: join this computer to a team (address and token), or leave it; sign in as a
person (the sign-in opens in the browser, so the organization's single sign-on works), or sign
out. Signed in, the home window lists the team's computers the person may reach; opening one gets
a ticket and connects. With an address book passphrase set, the person's own list of computers is
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
  its password); a code works once. Tried against LDAP when it is set up and the user is not
  local. Five wrong tries from one address (an IPv6 one by its /64), or twenty at one account,
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
- `GET /api/devices` -> `{"devices": [{"id", "name", "os", "group", "online": bool, "rights", "ask"}]}`:
  the devices the person may reach (online: seen in the last 3 minutes).
- `POST /api/ticket` `{"device": ID, "key": HEX32, "viewer": ID?}` -> `{"ticket": B64URL, "tag": HEX, "expires"}`
  where `key` is the viewer's device key (X25519, public) and `viewer` its own remote-access ID if
  it has one. 403 when no rule lets the person reach it.
- `GET /api/book` -> `{"book": B64 or null, "salt": HEX}`; `PUT /api/book` `{"book": B64}`: the
  person's own address book, sealed on the device (ChaCha20-Poly1305 with a key from PBKDF2 of
  their passphrase and `salt`); at most 1 MB.

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
`login`, `ticket`, `join`, `leave`, `removed`.

### Administration (an administrator's token)

- `GET|POST /api/admin/users`, `PUT|DELETE /api/admin/users/NAME`
  (`{"name", "display", "email", "admin", "groups", "password"?, "password_clear"?, "disabled", "totp"?, "totp_reset"?}`;
  `"totp": true` makes an authenticator and answers with `totp_secret`, `totp_uri` and `totp_qr`
  once). Single sign-on finds a person by the issuer and subject it linked, else by an email the
  provider says it verified, in an allowed domain, and links it only to an account with no
  password, authenticator or administrator's rights.
- `GET|POST /api/admin/groups`, `DELETE /api/admin/groups/NAME`; the same for `/api/admin/device-groups`
  (a device group has a `policy`).
- `GET /api/admin/devices`, `PUT|DELETE /api/admin/devices/ID` (`{"name", "group"}`; `{"end": true}`
  ends its sessions at its next look).
- `GET|PUT /api/admin/rules` (`[{"who": "user:NAME"|"group:NAME"|"all", "devices": "group:NAME"|"device:ID"|"all", "rights": u8, "ask": bool}]`).
- `GET|POST /api/admin/tokens`, `DELETE /api/admin/tokens/ID` (`{"group", "expires", "uses"}` -> with `"token"`).
- `GET /api/admin/audit?from&to&device&user&kind&limit` -> `{"events": [EVENT + "device"]}`;
  `GET /api/admin/audit.csv` the same as CSV.
- `GET|PUT /api/admin/settings` (`{"name", "logo", "relay", "oidc": {"issuer", "client_id",
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

### Installers

`GET /install.sh?token=T` and `GET /install.ps1?token=T`: the official one-line installers from
sshremote.dev, then `sshremote team join URL T` and remote access turned on with the service: a
computer that runs one is in the team.

### Ticket

`ticket` is base64url of compact JSON `{"v": 1, "d": DEVICE_ID, "u": NAME, "n": DISPLAY,
"r": RIGHTS, "a": ASK, "k": VIEWER_KEY_HEX, "i": VIEWER_ID, "e": EXPIRES, "x": NONCE_HEX}` and
`tag = HMAC-SHA256(team key of DEVICE_ID, "sshremote team ticket\n" + ticket)`. The viewer sends
`ticket` in the handshake (flag `TEAM`) and runs the password exchange with the tag as the
password; the computer computes the tag itself, refuses an expired ticket or one for another
ID, then asks for the viewer's device key proof (the `WHO` step) and lets it in only with the
key the ticket names.
