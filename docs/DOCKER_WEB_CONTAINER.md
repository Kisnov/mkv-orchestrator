# MKVO Docker Web Container

The Docker web build runs MKV Orchestrator as one container:

- The Rust/Axum server hosts the API and serves the web app.
- React, TypeScript, Vite, Tailwind CSS, TanStack Query, React Router, and lucide-react provide the browser UI.
- MKVToolNix and FFmpeg are installed inside the runtime image.

## Run

CI publishes the image as `ghcr.io/kisnov/mkv-orchestrator:latest` (and
`:sha-<short commit>`); `docker-compose.yml` uses it. Pull and start it:

```powershell
docker compose pull
docker compose up -d
```

Or build it from this checkout with `docker compose up --build`.

Open:

```text
http://localhost:8886
```

Sign in as `admin`. Unless `MKVO_PASSWORD` is set, the password is generated on
first start; read it with `docker exec mkvo cat /config/admin-password.txt`.
See [authentication notes](AUTHENTICATION.md), including the upgrade steps for
an install that ran without login.

## Volumes

The default compose file uses local bind mounts:

```text
./tmp/docker-media     -> /media
./tmp/docker-downloads -> /downloads
./tmp/docker-config    -> /config
```

The web app browses container paths. With the default compose file, `/media` and `/downloads` point at local folders under `./tmp`.

Set these values in a local `.env` file to customize bind mounts:

```text
MKVO_MEDIA_PATH=D:/Media
MKVO_DOWNLOADS_PATH=D:/Downloads/Media
MKVO_CONFIG_PATH=./tmp/docker-config
MKVO_SOURCE_ROOTS=downloads=/downloads
```

## Environment Variables

| Variable | Default | Purpose |
| --- | --- | --- |
| `MKVO_MEDIA_ROOT` | `/media` | Default root shown by the Dashboard browser. |
| `MKVO_SOURCE_ROOTS` | `downloads=/downloads` | Additional named source roots (`name=/path`, comma separated). |
| `MKVO_SCAN_WORKERS` | `6` | Maximum concurrent metadata scans (clamped 1-8). |
| `MKVO_EDIT_WORKERS` | `2` | Maximum concurrent mkvpropedit edits (clamped 1-6). |
| `PUID` / `PGID` | `1000` / `1000` | Run the app as this user/group so files written to shares are not root-owned. Unraid users typically want `99` / `100`. `0` / `0` runs as root (see Security Notes). |
| `UMASK` | unset | File creation mask applied at startup. |
| `MKVO_AUTH_ENABLED` | true | In-app administrator login. False gives everyone who can reach MKVO full access, including reverse-proxy visitors, and needs `MKVO_ALLOW_NO_LOGIN=true` as well. |
| `MKVO_ALLOW_NO_LOGIN` | false | Confirms running without login on a non-loopback address; otherwise the server refuses to start. |
| `MKVO_USERNAME` / `MKVO_PASSWORD` | admin / generated | Use a password of at least 12 characters. Blank generates one in `/config/admin-password.txt`, replaced by `admin-password.hash` after the first sign-in. Never publish either file. |
| `MKVO_ALLOWED_HOSTS` | unset | Extra host names (comma separated) accepted without login and as request origins; IP addresses and `localhost` always work. Needed for a host name with login off, or behind a proxy that rewrites `Host`. |
| `MKVO_TOOL_DIRECTORIES` | unset | Directories the Settings page may choose for MKVToolNix/FFmpeg. The image finds its bundled tools on its own, so leave the Settings fields blank; any other directory is refused. |
| `MKVO_SECURE_COOKIES` | false | Enable for HTTPS; leave false for plain HTTP. A proxy sending `X-Forwarded-Proto: https` gets a Secure cookie anyway. |
| `MKVO_AUTH_MODE`, `MKVO_AUTH_USERNAME`, `MKVO_AUTH_PASSWORD` | legacy | Existing settings are honored unless their new equivalents are explicitly set; blank values count as unset. |
| `MKVO_TVDB_API_KEY`, `MKVO_TVDB_PIN`, `MKVO_TMDB_API_KEY` | unset | Optional provider credentials; Settings-page values take effect otherwise. |

Container-provided provider credentials appear as configured in the Settings
page after startup, but their values are never returned to the browser or
copied into `secrets.json`. Update the container variable and restart the
container to rotate or remove one. An environment value takes precedence over
a value entered through the web UI.

## Security Notes

- Filesystem browsing through the web UI is limited to the configured source
  roots, watch folders, and enabled media-server library paths. Arbitrary
  container paths are rejected.
- Login is on by default. The port is published on every host interface so
  the LAN can reach it; use `127.0.0.1:8886:8080` for local-only access, and
  HTTPS plus network restrictions before exposing MKVO beyond a trusted network.
  See [authentication notes](AUTHENTICATION.md).
- The compose file and Unraid template drop every Linux capability except
  `CHOWN`, `SETUID` and `SETGID` (the entrypoint fixes `/config` ownership and
  switches to `PUID`/`PGID`) and set `no-new-privileges`. With `PUID=0` the app
  runs as root without `DAC_OVERRIDE`/`FOWNER`, so it cannot write files owned
  by other users; add those two capabilities or use a real PUID/PGID.
- Media-server URLs must be HTTP(S) and cannot use link-local addresses
  (`169.254.0.0/16`, `fe80::/10`). LAN addresses and container names work.
- Manual subtitle files for muxing must be inside the source roots.

## Long-Running Operations

Mux/remux and track property applies run as background jobs inside the
container. The web UI polls job progress (per-file percent for remuxes), can
cancel a running job, and survives page refreshes without dropping the
operation. Results are also written to the Logs page.

## Watch Folders

Watch folders configured in Settings are monitored live inside the container
when **Enable live watch-folder monitoring** is on: new or changed media files
are re-scanned into the metadata cache automatically and deleted files are
pruned. Cache entries not refreshed for 30 days are removed at startup.

## NAS / SMB Shares

For a NAS share, copy `.env.example` to `.env`, edit the share paths, then run Docker Compose with the NAS example override:

```powershell
docker compose -f docker-compose.yml -f docker-compose.nas.example.yml up --build
```

The NAS example uses Docker's local CIFS volume driver:

```yaml
volumes:
  mkvo-media:
    driver: local
    driver_opts:
      type: cifs
      device: ${MKVO_MEDIA_SHARE}
      o: ${MKVO_CIFS_OPTIONS}
```

If your share requires credentials, put them only in your local `.env`.

Example:

```text
MKVO_MEDIA_SHARE=//server/media
MKVO_DOWNLOADS_SHARE=//server/downloads/media
MKVO_CIFS_OPTIONS=username=myuser,password=mypassword,vers=3.0
```

Do not commit `.env`, SMB usernames, SMB passwords, API keys, or server-specific paths.

## Unraid Template

The canonical Community Applications template is:

```text
templates/my-mkv-orchestrator.xml
```

The lowercase `my-mkv-orchestrator.xml` filename follows the Unraid template
naming convention. The display name remains `MKV Orchestrator`, while the image
and repository slug remain `mkv-orchestrator`. Environment variables use the
existing `MKVO_` prefix. `ca_profile.xml` in the repository root supplies the
required Community Applications repository profile. Keep this as the single
canonical template so Community Apps does not discover duplicate entries for
the same application.

The template uses generic defaults:

```text
/mnt/user/ -> /media
/mnt/user/ -> /downloads
/mnt/user/appdata/mkv-orchestrator -> /config
```

Adjust those host paths in Unraid before starting the container. Existing users
can keep an earlier `/mnt/user/appdata/mkvo` mapping to retain their data. The
template does not include API keys, SMB credentials, or server-specific paths.
TVDB/TMDB keys can be entered in Settings after first launch, or through the
masked optional environment variables. Require Login defaults to true; read the
generated password from `admin-password.txt` in the appdata folder, or fill in
Administrator Password. Setting Require Login to false also needs the advanced
Allow No Login set to true.

Before submitting or updating the template, run the Community Applications
**Validate** and **Scan** checks after every meaningful XML change.

## Included Tools

The image installs:

- `mkvmerge`
- `mkvpropedit`
- `mkvextract`
- `mkvinfo`
- `ffmpeg`
- `ffprobe`

The Settings page reports the resolved tool paths and version strings from inside the container.

## Current Scope

This is a single-container web release. Dashboard scanning, rename preview/apply/undo, mux/remux planning, lossless MP4 to MKV conversion, track property edits, library overview, settings, and logs are exposed through the same Rust host. Processing logic remains in shared core services instead of being duplicated in the React UI.
