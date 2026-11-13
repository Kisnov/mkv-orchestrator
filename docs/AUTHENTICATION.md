# Administrator login

MKVO ports the authentication components from PosterView revision
`6e8621b70ce43ddb14f20e95ad3a5488af4c5ad7`, adapted to MKVO branding,
theme tokens, storage names, and existing routes. This is single-administrator
authentication, not multi-user accounts, MFA, or password recovery.

## Container setup and compatibility

Login is on by default everywhere: the standalone server, `docker-compose.yml`,
`.env.example` and the Unraid template (Require Login = true). Changing
environment settings requires recreating the container.

### Signing in the first time

The username is `MKVO_USERNAME` (case-sensitive; blank uses `admin`). The password is:

- `MKVO_PASSWORD` when it is set. Use at least 12 characters; a shorter one
  still works but logs a warning at every start. It is kept in memory only.
- Otherwise a random password generated into `/config/admin-password.txt`
  (owner-only permissions on Unix). Read it with
  `docker exec mkvo cat /config/admin-password.txt`, or open the file in the
  config folder on the host (Unraid: `/mnt/user/appdata/mkv-orchestrator`).
  It is never written to the logs; the startup log says which file to read.

After the first successful sign-in with the generated password, MKVO replaces
`admin-password.txt` with `admin-password.hash` (an Argon2 hash), so the
plaintext no longer sits in the config folder. Keep the password in a password
manager. To get a new one, delete `admin-password.hash` (and
`admin-password.txt` if present) and restart; or set `MKVO_PASSWORD`, which
always wins. A password typed into `admin-password.txt` by hand is accepted
and hashed the same way.

### Running without login

`MKVO_AUTH_ENABLED=false` (or the legacy `MKVO_AUTH_MODE=disabled`) grants full
access to everyone who can reach the service, including visitors through a
reverse proxy. On any bind address other than loopback the server refuses to
start unless `MKVO_ALLOW_NO_LOGIN=true` is also set, and the startup error says
so. Without login, API requests are answered only for IP addresses, `localhost`
and the names in `MKVO_ALLOWED_HOSTS` (see below), which stops DNS rebinding.

### Upgrading an install that ran without login

Earlier container definitions defaulted to login off. After upgrading:

- If your compose file or `.env` sets `MKVO_AUTH_ENABLED=false` or
  `MKVO_AUTH_MODE=disabled`, the container stops with an error that names these
  variables. Remove the setting to turn login on, or add
  `MKVO_ALLOW_NO_LOGIN=true` to keep it off.
- If it relied on the old default, login is now on. Sign in as `admin` with
  the password from `/config/admin-password.txt` (it already exists if the
  container ran before), or set `MKVO_PASSWORD` and recreate the container.

Credentials are retained while login is disabled. `/media` and `/config` are
unchanged. Legacy `MKVO_AUTH_MODE`, `MKVO_AUTH_USERNAME` and `MKVO_AUTH_PASSWORD`
are still honoured; nonblank new variables override them, and an explicit
`MKVO_AUTH_ENABLED` overrides the legacy mode. Blank values count as unset.

The native Tauri desktop app continues to use the operating-system account;
the new login gate and server security preferences apply to the HTTP app only.
The existing desktop layout and zoom behavior are unchanged. Sign out sits
above the sidebar status panel, with header access at narrow browser widths.

### Failed sign-ins

After 5 failed attempts in a row, sign-in is locked for 30 seconds for every
client, doubling with each further failure up to 15 minutes (`429` with
`Retry-After`). A successful sign-in or an hour without failures resets it.
The lock is global on purpose: a per-address limit is defeated by changing
address. The cost is that someone guessing can also keep you out for a while;
wait for the lock to expire, or restart the container to clear it.

## Sessions and preferences

The public login, logout, and status endpoints use same-origin requests.
All other APIs except health are protected when a password is required.
The configured username is intentionally public for prefill; passwords are not.
The opaque session cookie is named `mkvo_session`, HttpOnly, SameSite=Strict,
Path=/, and optionally Secure. Sessions stay in memory and end on restart.

Privacy / Security persists timeout and LAN-bypass preferences atomically in
`/config/security-settings.json`. Inactivity logout defaults to disabled;
enabled timeouts range from 1 to 1440 minutes. The server independently enforces
expiry. Only activity requests extend sessions, not background API polling.
Username remembering is browser-local, defaults on, and saves immediately.
No password or session token is stored in localStorage or sessionStorage.

Local-network bypass defaults off and uses only the actual TCP peer address,
not Host or forwarded headers. Missing peer information fails closed. Any
`Forwarded`, `X-Forwarded-*` or `X-Real-IP` header turns the bypass off for
that request, because it means a proxy relayed it and the peer is the proxy.
In Docker bridge networking (the default) visitors may appear as the bridge
gateway, a private address, so do not enable the bypass there unless every
host that can reach the published port is trusted.

> Warning: Anyone whose connection appears local gets full access without a password.
> A reverse proxy or Docker networking can make remote visitors appear local too,
> including visitors using your public domain. Enable only if you accept this risk.

## Requests from other sites, headers and limits

- State-changing requests (anything but GET/HEAD/OPTIONS) that carry an
  `Origin` header must come from the host the page was served from: the
  `Host` header, or `X-Forwarded-Host` behind a proxy. A proxy that rewrites
  `Host` without sending `X-Forwarded-Host` needs the public name listed in
  `MKVO_ALLOWED_HOSTS` with its public port, for example
  `mkvo.example.com:443`. A listed name without a port is accepted as an origin
  only on the port MKVO is being served on, so another web app on the same
  host (say `http://nas.local:9000`) cannot send requests with your session.
  Clients without `Origin`, such as curl, are unaffected.
- `MKVO_ALLOWED_HOSTS=*` switches both checks off: any web page you visit can
  then send requests to MKVO from your browser, and with login off a rebinding
  page can read and change everything. Use it only on a network you fully trust.
- Every response carries `Content-Security-Policy` (same-origin scripts only,
  no framing), `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff` and
  `Referrer-Policy: same-origin`.
- Requests are stopped after 120 seconds, except rename, mux/remux, track
  property, library and media-server endpoints, which can legitimately run for
  many minutes and are never cut halfway.
- The session cookie gets `Secure` when `MKVO_SECURE_COOKIES=true` or when the
  sign-in arrives through a proxy that sends `X-Forwarded-Proto: https`.
- Error messages may include absolute container paths. They are only returned
  after sign-in (or with login off) and help diagnose mounts, so they are kept.

Global login disabled overrides bypass and inactivity settings. Password-free
connections have no sign-out control or inactivity lock. Keep TLS, network
restrictions, and reverse-proxy protections in place as appropriate.

## Verification for this port

- 137 frontend tests and 32 server tests pass; frontend production build passes.
- Server formatting and strict server-only Clippy pass. The workspace-wide
  formatting check and dependency-inclusive strict Clippy still report existing
  formatting / collapsible-if issues outside the authentication changes.
- Unraid XML parses and Docker Compose validates.
- The actual server entry point was exercised over loopback TCP for login,
  logout, and opt-in local-peer bypass.
- The login screen was inspected at desktop and 390px phone width; Security
  settings were inspected at desktop width. The full mobile console was not
  redesigned or visually certified. Linux container execution and Unix password
  permissions still require a Linux/container verification run.
