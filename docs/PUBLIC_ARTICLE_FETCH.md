# Public WeChat article fetch boundary

GreenBubbles can parse one ordinary public WeChat article URL through the
separate `greenbubbles-public-article` executable. This optional Phase 5A helper
is not part of restoration, the encrypted replica service, or the Unix
connector.

That process boundary is intentional. The executable has no dependency on
`GreenBubblesCore`, no WeChat database passphrase, no replica key, no policy or
audit file, no source snapshot, and no logged-in client/session state. It
accepts a URL only from a regular, single-link, owner-only request file:

```json
{
  "formatVersion": 1,
  "url": "https://mp.weixin.qq.com/s/example-public-article"
}
```

```sh
chmod 600 /private/public-article-request.json
swift run greenbubbles-public-article /private/public-article-request.json
```

The example URL is a placeholder. Use only an article the user is normally
authorized to access. The JSON result contains the final URL, observation time,
normalized title/author/description when present, extracted text, its UTF-8
byte count, and access evidence. It labels completeness `singlePublicPage`; it
does not claim an authoritative or complete public-account archive.

## Current operational status

The official [`robots.txt`](https://mp.weixin.qq.com/robots.txt), observed
through an unauthenticated HTTPS GET on 2026-08-27, returns HTTP 200 and applies
this controlling shape to all agents:

```text
User-Agent: *
Allow: /$
Allow: /debug/
Allow: /qa/
Allow: /wiki
Allow: /cgi-bin/loginpage
Allow: /cgi-bin/wx
Allow: /webpoc/ruleCenter
Allow: /miniprogram/landing_page
Disallow: /
```

None of the allowed paths covers `/s` or `/s/...`. Under longest-match robots
semantics, the public-article path therefore matches `Disallow: /`. The helper
currently returns `robotsDenied` before issuing the article request. No article
was fetched during this validation. The implementation is retained so a future
published policy change can be handled without weakening the boundary, but
Phase 5A public-article parsing is not currently an available capability.

## Enforced fetch policy

The production transport:

- accepts only HTTPS, default-port `mp.weixin.qq.com` URLs whose path is `/s`
  or begins `/s/`;
- rejects embedded URL credentials and known session-style query names such as
  `key`, `pass_ticket`, `uin`, `wxtoken`, and `wx_header`;
- rejects URL user information and redirects to every other origin or path
  class;
- uses an ephemeral URL session with cookies, cookie storage, credential
  storage, and cache disabled;
- checks `robots.txt` before the article request and fails closed when the
  policy is malformed, unavailable, or denies the article path; an ordinary
  404/410 means no policy was published;
- follows at most three same-host HTTPS redirects;
- limits robots data to 64 KiB, the HTML response to 2 MiB, and extracted text
  to 512 KiB;
- accepts only HTTP 200 UTF-8 HTML for the article;
- rejects authentication status and visible paywall/paid-content markers;
- extracts only the supported public article container and never fetches its
  images, videos, styles, scripts, or other subresources.

The paywall check is intentionally conservative but cannot prove the absence of
every publisher-specific commercial restriction. Therefore, the tool must be
used only for a normally accessible public page. If the page needs login,
cookies, a token, a client session, a subscription, CAPTCHA completion, or a
different origin, this helper stops; those restrictions are not inputs to work
around.

## Copyright, retention, and AI use

Fetching a public URL does not transfer copyright or create permission to
republish it. The helper prints a transient local result and does not add it to
the canonical replica, cache it, crawl related pages, or redistribute it.
Operators are responsible for lawful use and retention.

Article HTML and extracted text are untrusted source content. Nothing in the
page can change URL policy, deterministic connector scopes, model destination,
or action capability. An agent host should treat the result as quoted source
material, minimize it before model use, and never interpret text in it as a
tool instruction.
