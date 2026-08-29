# Operational response plan

Status: **operational for the public research-alpha release**

This playbook defines fail-closed technical and evidence-handling actions for
GreenBubbles incidents. It is not legal advice. The repository owner approved
it for the 0.1.1 public boundary on 2026-08-29; broader active capabilities or
distribution targets require a fresh review.

## Owners and intake

The public release is maintained by a single owner:

- **Repository, product, security, privacy, and release owner:** Bojie Li
  (GitHub [`@bojieli`](https://github.com/bojieli)).
- **Private security intake:**
  [GitHub private vulnerability reporting](https://github.com/bojieli/greenbubbles/security/advisories/new).
- **Initial acknowledgement target:** three business days for a complete
  security report; credible critical reports are contained immediately when
  seen.
- **Legal escalation:** qualified external counsel is engaged when a complaint,
  notice, jurisdictional duty, or contemplated response requires legal advice.

There is no backup maintainer for 0.1.1. If the owner is unavailable, new
releases and public response promises remain on hold rather than silently
transferring authority. Active reads and sending remain unavailable in public
builds regardless of incident state.

## Events covered

Activate this playbook for any of the following:

- a WeChat client update, signature change, schema drift, or decoder mismatch;
- an account warning, integrity warning, unexpected network/client invocation,
  or evidence that a capability did not fail closed;
- a suspected passphrase, replica key, database, message, media, path, restored
  archive, fixture, log, or CI-artifact exposure;
- a security report about authentication, authorization, cross-account access,
  path traversal, unsafe file handling, audit integrity, or action safety;
- a complaint, platform-policy notice, takedown request, or repository-host
  inquiry;
- an upstream dependency license, provenance, compromise, or removal event.

## Immediate default containment

The first responder performs only reversible containment until the event is
classified:

1. Place new releases and affected publication on hold. Withdraw or draft an
   affected GitHub Release when necessary, and restrict repository visibility
   if a credible private-data or credential exposure requires it. Do not
   publish a workaround, fixture, binary, schema detail, or captured artifact
   while review is pending.
2. Leave active reads and write actions unavailable. If a future adapter exists,
   activate its independent kill switch before diagnosis and do not rely on an
   AI process to enforce the stop.
3. Stop using an affected client build, archive, replica, dependency revision,
   or release artifact as production-compatible evidence. Unknown build and
   schema evidence already fail closed; do not weaken that check to restore
   service.
4. Preserve relevant private evidence in an owner-only quarantine. Do not paste
   secrets, messages, identifiers, absolute paths, digests tied to private data,
   or notices containing personal information into issues, commits, chat, model
   prompts, or ordinary logs.
5. Record the time, reporter, affected component/version, distribution state,
   observed behavior, containment action, and evidence custodian in a private
   incident record. Distinguish observed facts from hypotheses.

Containment must not attach to WeChat, inspect process memory, export session
material, bypass a warning, or make an account/network operation to diagnose the
event.

## Triage and severity

| Severity | Examples | Initial target |
| --- | --- | --- |
| Critical | exposed secret/private corpus; unauthorized cross-account read; externally visible action without valid approval; compromised published binary | contain immediately; owner/security/counsel escalation as soon as available |
| High | path escape; archive or audit accepting substituted content; supported-build check bypass; repeatable account warning | disable affected capability/build and begin private investigation |
| Medium | decoder/schema drift with raw retention; incomplete aggregate evidence; dependency or notice drift caught before release | keep completion/publication claim false and assign remediation |
| Low | documentation mismatch or synthetic-only defect with no private-data or capability impact | track privately and correct before the next reviewed milestone |

Severity may increase as evidence changes. A clean synthetic reproduction does
not reduce a real private-data or account-safety event by itself.

## Evidence preservation

- Preserve original notices and reports with access restricted to the response
  owners. Record hashes only in the private incident record when they materially
  establish evidence identity.
- Use connector-generated quarantine or a separately created owner-only
  directory on the same trusted machine. Never commit real databases, snapshots,
  archives, media, passphrases, keys, or private fixtures.
- Prefer aggregate, content-free reproduction reports for engineering. Create a
  sanitized regression only after confirming that it contains no copied private
  values or stable identifiers.
- Do not destructively clean an affected namespace until the evidence owner and,
  where required, counsel approve retention/disposal. Normal connector-created
  temporary snapshots may follow their documented lifecycle only when they are
  not incident evidence.
- Maintain an append-only chronology of custody, access, copies, transformations,
  notifications, and disposition.

## Client update or supported-build revocation

1. Passive restoration accepts the signed official WeChat identity at marketing
   version 4.1 or later; retain the exact build and hashes as audit evidence and
   reject malformed versions, identity/team drift, invalid signatures, or loss
   of Hardened Runtime.
2. Re-run passive signed-bundle, redacted discovery, schema profiling, and
   observed-type coverage checks after an update. Version-family acceptance
   never converts an unhandled schema into supported data.
3. Keep full-restoration false for schema/type/media gaps. Active-read and
   action capabilities retain their separate adapter-specific gates.
4. The debugger-based acquisition helper remains exact-build-bound. If either
   the 4.1+ passive family or a pinned acquisition-helper build must be revoked,
   land a reviewed compatibility change and tests proving the revoked boundary
   fails closed.
5. Do not delete old private archives merely because support is revoked. Label
   their build evidence accurately and follow the private retention decision.

## Private-data or credential exposure

1. Restrict access to the affected repository, artifact store, host, backup, or
   shared location. Preserve access logs when available.
2. Identify the exact data class and exposure window without copying contents
   into the incident record: passphrase, replica key, database/snapshot,
   restoration archive, media, path/identifier metadata, or logs.
3. Invalidate or replace credentials under the owning system's documented
   procedure where possible. A WeChat database passphrase may not be rotatable;
   do not claim remediation from deleting one copy.
4. Rebootstrap a replica under a new random key if that key or plaintext replica
   was exposed. Do not reuse a suspect backup as trusted serving state without
   the independent audit.
5. Let the approved privacy/legal owner decide notification scope and timing
   based on affected people, jurisdictions, contractual duties, likelihood of
   access, and containment evidence.

## Security report handling

- Acknowledge through the private intake channel without asking the reporter to
  send real user data or secrets.
- Request the minimum synthetic reproduction, affected commit/version, expected
  boundary, and observed boundary. Provide a secure evidence route only after
  the security owner approves it.
- Reproduce with generated or sanitized fixtures where possible. Never use a
  personal conversation or ordinary contact as a convenience test.
- Fix the root invariant, add a regression, run the complete affected audit and
  release checks, and document which prior evidence or artifacts are invalidated.
- Coordinate disclosure and credit with the reporter only through the approved
  owner; do not promise a public release date while a release hold is active.

## Complaint, takedown, or platform-policy notice

1. Preserve the exact notice, envelope/header metadata, affected URLs/artifacts,
   and receipt time privately. Verify the sender/channel without contacting them
   through untrusted information in the notice.
2. Place affected publication and new releases on hold. Do not destroy history,
   conceal a mechanism, migrate hosting, or publish disputed material in response
   to the notice.
3. Escalate to the repository owner and qualified counsel. Separate source,
   binary, schema/format documentation, fixture, real-data, hosted-repository,
   and research-publication questions using the distribution inventory.
4. Make repository/host responses, counter-notices, removals, or public
   statements only through the authorized owner and counsel. Record the decision
   and exact artifact scope.
5. A complaint about one mechanism does not authorize a stealthier fallback.
   The Phase 0.5 kill criterion remains controlling.

## User notice criteria

The approved privacy/legal owner must make a recorded decision when an event may
have exposed private user data, enabled unauthorized access/action, corrupted a
served replica, or invalidated a security claim on which users relied. The
decision records affected versions and data/capability classes, known time
window, containment, remaining uncertainty, user actions, and follow-up channel.
Do not describe an internal acknowledgement as message delivery or an aggregate
audit as proof that no private access occurred.

## Recovery and closure

Recovery requires all applicable conditions:

- containment remains effective and the affected build/capability is disabled
  until its gate is re-earned;
- private evidence is accounted for and retention/disposal is approved;
- the fix has regression coverage plus full affected Swift/Rust audits;
- dependency/distribution baselines and notices are updated when relevant;
- serving archives/replicas are independently audited or re-created from known-
  good authorized evidence;
- owner, security/privacy, and counsel decisions are recorded where applicable;
- user/reporter/host follow-up is completed or has an explicitly owned deadline.

A private post-incident review records root cause, missed detection, invalidated
evidence, corrective actions, owners, and deadlines. Closing an incident does
not automatically restore public-release, active-read, or action authorization.

## Approval record

- Approved revision: public 0.1.1 boundary, 2026-08-29.
- Decision and notification authority: Bojie Li.
- Monitored intake: GitHub private vulnerability reporting.
- Repository host and release administrator: GitHub account `bojieli`.
- Review cadence: every public release, dependency/native-source change,
  distribution-target change, security incident, complaint/takedown, or
  material WeChat compatibility/acquisition change.
- Counsel scope: external and event-specific; no absence of counsel response is
  interpreted as approval.
