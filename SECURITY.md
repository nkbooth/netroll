# Security policy

NetRoll is maintained by one person. This page says what that person can
promise, and nothing more.

## Reporting a vulnerability

Email **nick@n1cck.us**. If you want to encrypt, the PGP key is available two
ways: your client can fetch it over Web Key Directory
(`gpg --locate-external-keys nick@n1cck.us`), or download `gpg-key.asc` from
the contact page at <https://n1cck.us/find-me/>. Whichever way you fetch it,
check the fingerprint:

```
8BC5 B31F 8596 1109 9609 66D6 1928 3067 2472 CAF5
```

The key expires on 2027-01-25; after that, look for its successor on the same
page.

You can instead open a private report on GitHub at
<https://github.com/nkbooth/netroll/security/advisories/new>. Either channel
reaches the same person. Please do not open a public issue for a
vulnerability.

## What to expect

You will get an acknowledgement within five business days. That is the one
promise on this page.

The aim is to have an assessment — is this real, how bad is it, what is the
plan — within fourteen days. That is a target, not a promise, and it is the
only date you will be given: there is no fix timeline, because the fix depends
on the finding.

You may disclose the issue publicly ninety days after your report, whether or
not a fix has shipped. That is your right, and you do not need permission to
exercise it. If a fix lands earlier and you would rather disclose then, say so
and we can coordinate.

## Supported versions

Only the latest release receives fixes. There is no long-term-support branch.
If you run an older version, upgrade first and check whether the issue is
still there.

## If a secret or an identifier was pushed

Two different problems, two different fixes.

**A credential was pushed** — an API key, a password, a token. Rotate it
immediately, before anything else. Treat it as compromised from the moment of
the push, not from the moment you noticed: assume it was read. Rewriting or
force-pushing history afterwards does not change that.

**An identifier was pushed** — a hostname, an internal path, a name that was
meant to stay quiet. It is public now. Force-pushing does not unpublish it;
forks, clones, caches and notification emails already have it. If the
identifier matters, change the identifier.
