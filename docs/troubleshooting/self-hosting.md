---
title: "Troubleshoot self-hosting"
description: "Fix a self-hosted NetRoll instance: containers that exit at boot, magic-link emails that never send, features returning 503, lost avatars, and blocked webhooks."
docType: "troubleshooting"
audience: ["self-hoster"]
lastVerified: "2026-09-14"
---

# Troubleshoot self-hosting

Problems running your own NetRoll instance. NetRoll validates its configuration at boot and
names the offending variable in the error, so start with the logs — that line usually identifies
the problem outright.

```bash
podman compose logs app | tail -30
```

## The container exits immediately at boot

**Symptoms:** The app container starts and stops. The log ends with a configuration error.

**Cause:** A required variable is missing, or an optional one is set to an invalid value. Both
are deliberate hard failures — a value that looks configured but silently does nothing is worse
than a refusal to start.

**Fix:** Match the error to the variable:

| Error mentions | Fix |
|----------------|-----|
| `DATABASE_URL` | Set a Postgres connection string. There is no fallback. |
| `MAIL_FROM` | Set the From address. There is no fallback sender. |
| `PUBLIC_BASE_URL` | Set it to the public origin your proxy serves, as an `https://` URL. There is no fallback when unset. Plain `http://` is accepted only for loopback hosts (`localhost`, `127.0.0.1`, `[::1]`); any other scheme, or an empty host, is refused. |
| `PORT` | Use a number from 1 to 65535. `0` is refused because it would bind an ephemeral port. |
| `STATIC_DIR` | Don't set it empty. The path must contain `index.html`. |
| `KEK` | Base64-encoded 32 bytes. Unset is fine; malformed is not. |
| `BOT_MITIGATION_SECRET` | At least 16 bytes. Unset is fine; too short is not. |
| `MAX_NETS_PER_USER`, `MAX_OWNERS_PER_NET` | Positive integers. Zero, negative, and non-numeric are refused. |
| `ADMIN_ACCOUNT_EMAILS` | Remove the blank or malformed entry — often a trailing comma. |
| `AVATAR_DIR` | Give a writable path. The directory is created and probed at boot. |
| `CSP_POLICY` | Don't set it empty, keep it to a single line of visible ASCII, and make sure it names at least one recognised CSP directive. Unset is fine — the app derives its own policy. |
| `CSP_REPORT_ONLY` | Exactly `true` or `false`. Unset is fine and means enforce. |

**Verify:** `podman compose logs app` shows a clean start, and
`curl -s http://localhost:8080/healthz` prints `{"status":"ok"}`.

See the [configuration reference](../reference/configuration.md).

---

## The app boots but no email is sent

**Symptoms:** The instance runs, sign-in requests report success, no email arrives.

**Cause:** The app validates `MAIL_FROM`'s presence at boot but can't validate that your relay
accepts it. Sign-in is a magic link, so this makes the instance unusable even though it looks
healthy.

**Fix:**

1. Check the app logs for SMTP errors. Credentials themselves are never logged.
2. Confirm `SMTP_HOST` and `SMTP_PORT` reach your relay from inside the container.
3. Confirm the relay is authorized to send as `MAIL_FROM`, and that the domain has SPF and DKIM
   for it.
4. If the relay needs no authentication, leave `SMTP_USERNAME` empty. Setting it makes the app
   attempt an AUTH exchange the relay may reject.

**Verify:** Request a magic link and confirm delivery, then follow it to sign in.

---

## A feature returns HTTP 503

**Symptoms:** The app works, but one endpoint consistently returns 503.

**Cause:** An optional feature whose configuration is absent. This is the designed degradation
path, distinct from a boot error.

**Fix:**

- **QRZ credential endpoints:** set `KEK` to a base64-encoded 32 bytes. Once set, don't rotate
  it casually — credentials sealed under the old key become permanently undecryptable.

**Verify:** The endpoint responds normally after a restart.

---

## The admin surface is unreachable

**Symptoms:** No admin endpoints work, and the app booted cleanly.

**Cause:** `ADMIN_ACCOUNT_EMAILS` is unset, or an address is listed but no account has
registered under it. Listing an address grants nothing until the account exists.

**Fix:**

1. Add the address to `ADMIN_ACCOUNT_EMAILS` and restart.
2. Sign in at least once with that address to create the account.

**Verify:** The admin endpoints respond for that account.

---

## Avatars disappeared after an upgrade

**Symptoms:** Uploaded avatars are gone; users show Gravatar defaults.

**Cause:** No volume mounted at `AVATAR_DIR`. Avatars are the only user data outside Postgres,
and a deploy replaces the container rather than updating it in place.

**Fix:**

1. Mount a named volume at the avatar directory, `/app/data/avatars` in the published image.
2. Bring the stack up again.

Avatars uploaded before the volume existed are not recoverable. Users can re-upload.

**Verify:** Upload an avatar, recreate the container, and confirm it survives.

---

## The app renders but the console shows CSP violations

**Symptoms:** The page loads, but the browser console reports `Content-Security-Policy`
violations — blocked scripts or styles, or analytics events that never send.

**Cause:** Your reverse proxy is also setting a `Content-Security-Policy`. The app sends its own,
and a browser given two enforces both: the effective policy is their intersection, which nobody
wrote.

**Fix:**

1. Remove the CSP from your proxy configuration. `curl -sI https://your-host/` should show
   exactly one `content-security-policy` header — the app's.
2. To diagnose without blocking anything, set `CSP_REPORT_ONLY=true` and restart: the same
   policy is sent as report-only, so the console shows what *would* be blocked while the page
   keeps working. **Report-only enforces nothing, `frame-ancestors 'none'` included.** That
   directive is the app's only anti-framing rule — it sets no `X-Frame-Options` — so while the
   flag is on, the page is frameable unless your proxy sets `X-Frame-Options: DENY` (the example
   Caddyfile does). Keep the window short, and remove the flag once the proxy is clean.

**Verify:** The console is quiet and, with analytics enabled, events reach your Plausible origin.

---

## The host port serves nothing

**Symptoms:** The container is running and healthy; connecting to the host port gives nothing.

**Cause:** The container side of the port mapping doesn't equal `PORT`.

**Fix:** Make them match. With `PORT=3000`, the mapping is `'8080:3000'`. If you change `PORT`,
change the mapping too.

**Verify:** `curl -sI http://localhost:8080/` returns 200.

---

## A webhook URL is rejected

**Symptoms:** Saving a delivery webhook fails with a specific message, or delivery never fires.

**Cause:** The egress client refuses URLs it isn't allowed to call. All outbound HTTP shares one
SSRF-guarded client, which is what makes it safe to let net owners type in a URL.

**Fix:**

1. Use HTTPS. Plain HTTP is refused.
2. Use a publicly-routable host. Loopback, link-local, private (RFC 1918 and ULA), and
   cloud-metadata addresses are all refused.
3. Remember the check runs again at delivery time against the pinned resolved IP, so a hostname
   that later resolves into a denied range is still blocked.

**Verify:** The URL saves, and closing a session delivers a signed POST to it.

---

## Rate limits behave oddly across replicas

**Symptoms:** Limits appear looser than configured.

**Cause:** The rate limiter is in-app and per-process. Running multiple app replicas multiplies
the effective limits, because each process counts independently.

**Fix:** Run a single app replica. NetRoll is designed for single-node operation at its expected
load. If you need multiple replicas, expect the limits to scale with them.

**Verify:** Limits behave as configured with one replica.

---

## Get help

File an issue at
[github.com/nkbooth/netroll/issues](https://github.com/nkbooth/netroll/issues).

Include the image tag you're running, the exact boot error line if there is one, and your
compose file with secrets redacted.

## Related pages

- [Configuration reference](../reference/configuration.md)
- [Self-hosting quickstart](../self-hosting/quickstart.md)
- [Deployment model](../self-hosting/deployment-model.md)
- [Upgrade an instance](../self-hosting/upgrade-an-instance.md)
