# Public article fetch

GreenBubbles can parse one ordinary public WeChat article URL through a
separate `greenbubbles-public-article` executable.

**It currently refuses to fetch anything.** The reason is below, and it is not
a bug.

## Why this is a separate executable

The process boundary is the design. This binary has no dependency on
`GreenBubblesCore`, no WeChat database passphrase, no replica key, no policy or
audit file, no source snapshot, and no logged-in client or session state. It
cannot become a path into your history, because it has no access to any of it.

It accepts a URL only from a regular, single-link, owner-only request file:

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

The result carries the final URL, observation time, normalized title, author
and description when present, extracted text and its byte count, and access
evidence. It labels completeness `singlePublicPage` — one page, not an
authoritative archive of an account.

## Why it currently refuses

The official [`robots.txt`](https://mp.weixin.qq.com/robots.txt), observed via
an unauthenticated HTTPS GET on 2026-08-27, returns HTTP 200 with this
controlling shape for all agents:

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

No allowed path covers `/s` or `/s/…`. Under longest-match robots semantics the
article path matches `Disallow: /`, so the helper returns `robotsDenied` before
issuing any article request. No article was fetched during that validation.

The implementation is kept so that a future published policy change can be
handled without weakening the boundary. It is not currently an available
capability, and it is re-checked before any explicitly requested fetch.

## What the transport enforces

If it ever runs, it accepts only HTTPS default-port `mp.weixin.qq.com` URLs
whose path is `/s` or begins `/s/`. It rejects embedded URL credentials,
user information, and known session-style query names (`key`, `pass_ticket`,
`uin`, `wxtoken`, `wx_header`), and rejects redirects to any other origin or
path class.

It uses an ephemeral URL session with cookies, cookie storage, credential
storage and cache all disabled; checks `robots.txt` before the article request
and fails closed if that policy is malformed, unavailable, or denies the path
(an ordinary 404 or 410 means no policy was published); follows at most three
same-host HTTPS redirects; limits robots data to 64 KiB, HTML to 2 MiB and
extracted text to 512 KiB; accepts only HTTP 200 UTF-8 HTML; rejects
authentication status and visible paywall markers; and extracts only the
supported public article container — never its images, video, styles, scripts
or any other subresource.

The paywall check is deliberately conservative and still cannot prove the
absence of every publisher-specific restriction. Use this only for a page you
can normally read. **If the page needs a login, cookies, a token, a client
session, a subscription, or a CAPTCHA, the helper stops — those are not inputs
to work around.**

## Copyright, retention and AI use

Fetching a public URL transfers no copyright and creates no permission to
republish. The helper prints a transient local result: it does not add the
article to the canonical replica, cache it, crawl related pages, or
redistribute it. Lawful use and retention are the operator's responsibility.

Article HTML and extracted text are **untrusted source content**. Nothing in
the page can change URL policy, connector scopes, model destination or action
capability. An agent host should treat the result as quoted source material,
minimize it before model use, and never read text inside it as an instruction.
See [AI_TOOL_BOUNDARY.md](AI_TOOL_BOUNDARY.md).
