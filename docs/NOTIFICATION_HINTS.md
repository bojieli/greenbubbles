# macOS notification hints

Notification delivery can reduce latency, but it cannot be the source of truth
for conversation restoration.

Apple's UserNotifications API lets an application manage its own notifications;
it does not provide a supported subscription to another application's
notification bodies. Accessibility automation may observe some Notification
Center UI state after the user grants Accessibility permission, but visibility,
Focus modes, notification grouping, dismissal, previews, and OS updates make it
incomplete. Reading Notification Center's private database would introduce an
unsupported, TCC-sensitive storage dependency and is not part of the passive
adapter.

GreenBubbles therefore uses this hierarchy:

1. database-directory filesystem events can wake the reconciler;
2. a future explicitly user-enabled Accessibility observer may provide another
   wake-up hint;
3. consistent database snapshots and canonical-ID reconciliation determine
   actual state;
4. periodic reconciliation recovers missed or duplicate hints.

`greenbubbles notification-hints` reports whether the current process already
has Accessibility trust. It does not prompt, inspect Notification Center, or
read notification contents. Regardless of trust, its completeness result is
always false.
