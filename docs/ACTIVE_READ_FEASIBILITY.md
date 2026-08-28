# Authenticated active-read feasibility

This document records the bounded, static Phase 5B assessment for the pinned
WeChat macOS client. It is an inventory of signed application metadata, not an
active-read implementation and not permission to interact with a live client.

## Scope and pinned evidence

The inspected build is:

- bundle and signing identifier `com.tencent.xinWeChat`;
- marketing version `4.1.13`, build `269579`;
- team identifier `5A4RE8SF68`;
- executable SHA-256
  `041f2632f8c9f4208f0b1ad26d574384e0b854952097a851f7d9c7c6f64a8542`;
- CodeDirectory SHA-256
  `c6b9f9587044784456eb96314f685c965fbd7d88bdacb72387284b8df551df4f`;
- `arm64` and `x86_64`, with Hardened Runtime and a valid deep signature.

The assessment read only the signed app bundle's property lists,
code-signing entitlements, component directory names, linked-library metadata,
and bounded symbol/string results. It did not launch WeChat, invoke a URL,
connect to a Mach or XPC service, attach to a process, inspect memory, capture
traffic, access a session credential, open a user database, or perform an
account operation.

The reusable command enforces the complete pinned fingerprint and fails closed
for another build:

```sh
swift run greenbubbles integration-surfaces
```

Its versioned JSON contains no absolute application path. Every boundary is
classified conservatively, and `authenticatedReadEvidence` is `notProven` for
every item. The top-level active-read state is `unavailable` with reason code
`noAuthenticatedHighLevelReadContractProven`.

## Static boundary inventory

| Surface | Observed metadata | What it establishes | What it does not establish |
| --- | --- | --- | --- |
| URL handlers | `xweixin`, `weixin`, and `wechat` | The system can hand inbound URLs to the main app. | A reply channel, message-history read, Moments read, or callable authenticated API. |
| Share extension | `com.tencent.xinWeChat.WeChatMacShare`, extension point `com.apple.share-services` | The system can hand selected URLs, files, images, and movies into a user-mediated share workflow. | General chat reads or a background send contract. |
| File Provider extension | `com.tencent.xinWeChat.WeChatFileProviderExtension`, extension point `com.apple.fileprovider-nonui`, enumeration enabled | WeChat participates in Apple's system-managed File Provider surface for its document group. | That arbitrary message attachments, conversations, or Moments can be enumerated by GreenBubbles. File Provider access remains governed by macOS and the extension's own contract. |
| Bundled XPC component | `com.tencent.xWechat.DebugHelper`, service type `Application` | An internal XPC bundle exists. | A stable third-party protocol, caller authorization, or a high-level content operation. No `MachServices` declaration was present in the inspected component property lists. |
| Main-app Mach lookup exceptions | `com.tencent.xinWeChat-spks` and `com.tencent.xinWeChat-spki` | The sandboxed main app is allowed to look up these internal service names. | That another process may connect, that the service accepts third-party callers, or that either name exposes messages or Moments. |
| Application group | `5A4RE8SF68.com.tencent.xinWeChat` on the main app, File Provider, and Share extension | Those Tencent-signed components can share an app-group container. | GreenBubbles membership in Tencent's signing group or authorization to use the container. |
| Data access allow-list | Tencent input method `com.tencent.inputmethod.wetype` | The main metadata contains a narrowly named system data-access exception. | A general external read interface. |
| Helper apps | `com.tencent.xinWeChat.WeChatHelper` and `com.tencent.flue.WeChatAppEx` | Internal helper applications are bundled and inherit the sandbox where declared. | A documented third-party IPC protocol. |
| Bundled/private frameworks | Includes `WCDYWrapper`, `andromeda`, `mmcronet`, `ilink*`, `roam_*`, and others; the main executable directly links `WCDYWrapper`, `andromeda`, and `mmcronet` in both architectures. | Internal implementation and networking/storage capabilities exist in the client. | A supported ABI, an authenticated read operation, or safety across client updates. |

The static executable also imports ordinary XPC-related platform machinery as
part of its broader dependency graph, but the bounded symbol/string search did
not reveal a self-describing, exported high-level message or Moments read
contract. Absence from a string search cannot prove that no internal operation
exists; it means there is no safe callable contract proven by this evidence.

## Feasibility conclusion

The official bundle exposes useful *inbound* and system-mediated integration
surfaces. None is evidence that GreenBubbles can ask the already logged-in
client to return chat history or load more Moments. A service name, private
framework, class name, or extension bundle is only an implementation clue until
all of the following are shown on a disposable account:

1. a high-level read-only operation and its exact input/output contract;
2. caller authorization without re-signing, injection, process attachment,
   memory access, reusable credential export, or weakened security controls;
3. deterministic account and content scoping;
4. an observable, automatic failure mode for unknown client versions;
5. acceptable supportability, account safety, and legal review.

Those conditions have not been met. Consequently, `load_more_moments`, active
message reads, and all sends remain unavailable. Passive database restoration
and passive cached-Moments reads are independent and do not inherit authority
from this inventory.

## Next gate

No live prototype should begin until Phase 0.5 has a disposable test account,
explicit user-mediated experiment scope, and the legal/supportability review
called for by the plan. The prototype must stop if it encounters caller checks,
anti-tamper controls, credential requirements, version ambiguity, or any need
to operate on an ordinary contact. A negative result is a valid outcome and
must not be converted into a stealth or protocol-spoofing approach.
