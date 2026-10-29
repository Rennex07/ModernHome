# Pumpkin 0.2.1 API audit

Target: stable `0.2.1+26.3-26.51` (`224f9f814`), WIT **v0.1**. ModernHome keeps its existing internal plugin ID and data directory.

| Area | Result |
| --- | --- |
| Player lookup | Already uses online name lookup for admin commands and UUID lookup for countdowns. These are not offline profile lookups. |
| GUI and items | Uses native GUI/item APIs. No close-inventory or active-window-ID method was found; the existing Java packet workaround remains. Bedrock is not given guessed Java packets. |
| Teleport | Uses the player teleport API, which consumes its world handle and returns no success/cancellation result. No destination-chunk preload API was found. |
| Scheduler | Each callback now advances only its own viewer. Completion takes the destination before cleanup, and completion/cancellation/offline paths cancel the repeating task. Previously, destinations were discarded before teleport and multiple timers advanced all viewers. |
| Permissions | Explicit permission checks and bounded home-limit probing are used. Enumeration of player grants is not exposed. |
| Messaging | Chat/actionbar, item text and sounds use host APIs; existing configured messages are preserved. |
| Persistence | JSON is stored in the plugin data preopen. No offline profile, shared-filesystem migration or cross-server socket transport was added. |

The 152 native tests cover pure behavior, not live WIT callbacks. The Wasm component loads on a scratch 0.2.1 server.

Manual checks still required: run simultaneous countdowns for two non-admins; each should count once per second and reach the correct home. Test movement/combat cancellation and logout. Verify no success message appears without a destination, GUI controls work, and homes survive restart. Admin teleports bypass the countdown and do not exercise this fix.
