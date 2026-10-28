# PumpkinHome

A port of **AdvancedHome** from Paper to a Pumpkin plugin. Same commands, same two
screens, same messages, same sounds, and the same rules about when a teleport may be
refused. The layout and configuration files carry over unchanged, so a server
moving over keeps its setup.

Built against Pumpkin `0.2.0+26.3-26.51`, commit `204a94ed8`, plugin WIT tree
`e0bb97ea48d9857c181356ba14964d47b20d0f56` — the same revision PumpkinTAB,
PumpkinPerm and PumpkinPIE are pinned to.

```
cargo build --release --target wasm32-wasip2
# -> target/wasm32-wasip2/release/pumpkin_home.wasm
```

Drop that file in the server's plugin directory and start it. It asks for
`fs.read.data` and `fs.write.data` on first load; the host will not preopen the data
folder without them.

## Commands

| Command | Aliases | Permission | Does |
|---|---|---|---|
| `/home` | `/homes`, `/h` | — | Opens the homes screen |
| `/sethome` | — | — | Sets a home in the lowest free slot |
| `/delhome <n>` | `/deletehome`, `/rmhome` | — | Deletes a home |
| `/ahome <reload\|help\|see\|osee>` | — | `home.admin` | Admin commands |

## Configuration

Written once on first load and never rewritten, so your comments survive.

| File | What it holds |
|---|---|
| `config.yml` | `teleport-time`, `combat-delay`, chat/actionbar switches, `blacklisted-worlds`, `home-limit-scan-max` |
| `messages.yml` | Every message, unchanged from the Paper plugin |
| `sounds.yml` | The five sounds, unchanged |
| `gui/homes.yml` | The homes screen layout and items, unchanged |
| `gui/confirm.yml` | The delete confirmation screen, unchanged |
| `server.yml` | `server-id` — which server a home record belongs to |
| `database.yml` | `store.file`, the JSON file the homes live in |

`/ahome reload` re-reads all seven. A file that fails to parse is reported and **not**
installed — the previous configuration stays in force, because reverting to defaults
would quietly flatten every message and every limit at once.

## Permissions

Two nodes, and both are the Paper names, unchanged:

- **`home.admin`** — unlimited homes, and `/ahome`. Also any operator, which is what
  `plugin.yml`'s `default: op` meant.
- **`advancedhomes.limit.N`** — the largest N you hold is your limit. Nobody holds one
  by default, so everybody has one home.

They are granted by **PumpkinPerm**, which is what owns permissions here. Add them to
a group:

```yaml
groups:
  vip:
    weight: 10
    permissions:
      - "advancedhomes.limit.10"
  staff:
    weight: 50
    permissions:
      - "home.admin"
```

PumpkinPerm hands a group's `permissions:` entries to the host verbatim and the host
matches a player's own grants before it consults the registry, so these resolve with
no plugin-to-plugin call. This plugin names PumpkinPerm nowhere in its `dependencies`
— it works without it, and so does PumpkinPerm without this.

**Why scanning:** Pumpkin has no way to *list* a player's permissions, only to ask
about one at a time. The limit is found by probing `advancedhomes.limit.1` through
`home-limit-scan-max` (default 256) and taking the highest that is granted. Paper read
the whole list in one call and had no such ceiling. Raise it if you grant a limit
above it; it costs one lookup per GUI open, not per tick.

## What this plugin does *not* use, and why

Four things in AdvancedHome are not reachable from a WebAssembly component. Each was
read out of the host's source, not assumed:

1. **No SQL.** The sandbox is granted exactly one directory — the plugin's own data
   folder (`wasm_host/mod.rs:382-394`) — and neither a SQLite driver nor a MySQL
   client links into a component. The homes live in one JSON file, same row shape as
   the `player_homes` table.

2. **No Redis.** The plugin world imports no socket interface
   (`pumpkin-plugin-wit/v0.1/plugin.wit`), so `wasi:sockets` is not in the component.
   `server-id` and each home's `server_id` survive, so a home still records which
   server it belongs to and a cross-server home still shows its server line and
   reports itself unavailable to teleport. The transport is gone.

3. **No EssentialsX import.** `plugins/Essentials/userdata` is outside the one
   preopened directory, so `migrate-from-essentialsx` has no counterpart and is gone
   from `config.yml`.

4. **No closing a screen.** There is no `Player::close_inventory`. Where Paper closed
   the screen before teleporting, renaming or confirming a delete, this does not; the
   client closes it when the next screen opens.

Smaller ones: `Player::is_op` does not exist (the op manager answers it); a chunk
cannot be pre-loaded before a teleport, so a home in a never-generated column may
arrive in the void; and `/ahome osee` cannot reach an offline player, because there is
no offline lookup — it answers `admin-player-not-found`.

## Three deliberate departures

Everything else is copied. These three are not, and each is a place where copying
would have been worse:

- **A rejected rename still ends the rename.** Paper took its pending-rename entry
  off *before* it validated the message, so an empty or over-long name ends the wait
  and the player must right-click again. Reproduced.
- **Placeholders are substituted in one pass.** Paper walked its placeholder map
  calling `String::replace` once per entry, so a *value* was substituted again if it
  contained another key — a home named `%player%` would be rewritten on screen. One
  pass cannot do that, and the difference only shows when a value contains a
  placeholder, which is the case the old behaviour got wrong.
- **`/delhome`'s tab completion is bounded.** Paper suggested `1..=getMaxHomes`, which
  for an admin — `Integer.MAX_VALUE` — is two billion strings and an out-of-memory.
  Here it is `1..=min(max_homes, home-limit-scan-max)`: identical for everyone with a
  finite limit, bounded for the rest.

## Tests

```
cargo test    # 139 tests
```

They cover the parts that can be wrong silently and are pure: slot arithmetic,
pagination, click routing, the countdown state machine, the layout parsers, the
message and placeholder rules, and the store's file handling.

They do **not** cover `screens.rs`, `lib.rs`, `teleport::start` or anything else that
builds an `ItemStack` — off `wasm32` those wit imports are `unreachable!()` stubs, so
a test that touched one would abort the harness. That needs the server:

## Host behaviours this plugin has to work around

Three of these cost real debugging time; each is a property of the host, not of this
plugin, and each is easy to reintroduce.

- **`open_gui` fired `InventoryCloseEvent` re-entrantly.** When the open screen is not
  the player's own inventory, `Player::open_handled_screen_direct`
  (`entity/player.rs:5867`) called `close_handled_screen()`, which fires the close
  event **synchronously, back into this plugin**, before the new open packet is sent.
  A close handler that always clears the session therefore wiped the session for the
  screen that is on its way, and the screen opened once and then ignored every click.
  `state::begin_reopen`/`end_reopen` bracket each `open_gui` and `OnClose` skips a
  marked viewer.
- **`open_gui` also reset the player's cursor.** `close_handled_screen()` sends
  `Close Container` and then does exactly what `on_handled_screen_closed()` does, so
  re-opening an already-open screen emitted `Close Container` → `Open Screen`. The
  client reads that pair as "close the window, then open another", which grabs and
  re-releases the OS cursor and warps the pointer to the window centre — the whole of
  the cursor jump, on every refresh. **Fixed in the host**, not here: `open_handled_screen_direct`
  now calls `on_handled_screen_closed()` and sends no close packet. This needs a
  Pumpkin server built from the patched source, not just a new plugin.
- **Closing the screen on a rename is a hand-written packet.** Paper called
  `viewer.closeInventory()`, and there is no `close-inventory` in the v0.1 WIT. But
  `java-player.send-packet` takes the whole `clientbound-packet` enum and
  `Close Container` is one of its variants, so `screens::close_screen` writes it
  directly. Nothing in the WIT reports the open window's id, so it sends the close
  for every id in `0..=100` and lets the client discard the ones that do not match.
  A Bedrock player has no `as-java` and closes their own screen.
- **Only `PlayerChatEvent` is ever fired.** `AsyncPlayerChatEvent` is in the WIT and
  nothing in the server constructs it (`net/java/play/chat_message.rs:45`,
  `net/bedrock/play/chat_message.rs:19`). A handler registered for one is registered
  and never dispatched, which is why the rename prompt appeared and then did nothing.
- **A session is dropped on close, but a pending rename outlives it.** In Paper the
  chat listener *was* the GUI handler — `HomeGuiHandler.onChat` read `targetUuid`,
  `targetName` and `homes` off its own fields, and `onClose` deliberately left that
  handler registered while a rename was pending (`HomeGuiHandler.java:492`). The
  prompt tells the viewer to close the screen before typing, so the target has to
  travel inside `state::PendingRename`; read back out of the session, an admin
  renaming somebody else's home would rewrite their own.

Two more that changed behaviour rather than causing a fault:

- **`allow_grab_items`/`allow_put_items` are left alone.** Paper set neither; it
  cancelled every click through `InventoryClickEvent`, which `OnClick` still does.
  They were briefly blamed for the cursor jump and the blame was wrong: both are read
  in exactly one place — the `on_slot_click` guard at `entity/player.rs:6256-6301` —
  and every read is `if !flag { cancel(); return }`. Nothing serialises them, so they
  never reach the client and cannot move a pointer.
- **Item text is explicitly non-italic.** A client renders an item's display name and
  lore in *italic* unless told otherwise, and Bukkit's `ItemMeta` always said
  otherwise — `setDisplayName`/`setLore` apply `italic(false)` before the string is
  parsed. A legacy `&`-coded string carries no such instruction, so every name and
  lore line came out slanted. `items::plain` applies it to item text only; chat is
  not item text and is not italic either way.

## Manual verification on a server

1. Load it; confirm the data folder gets all seven files and `homes.json` appears
   after the first `/sethome`.
2. `/home` — the shipped screen should show beds in slots 11, 13, 15, 28, 30, 32, 34
   and dyes in 20, 22, 24, 37, 39, 41, 43, with the arrows on 45 and 53.
3. Left-click an empty bed → `Home set.`; right-click the home's dye → confirm; press
   the lime pane → gone.
4. Right-click a home's bed, type a name in chat, and check the item. Try an empty
   name (expect `Name cannot be empty!`) and a 40-character one (expect the trimmed
   message).
5. Left-click a home → the countdown runs with the actionbar and the timer sound;
   move and it cancels with `Teleportation cancelled.`
6. Give yourself `home.admin` through PumpkinPerm and confirm the delay is skipped and
   the limit becomes unlimited.
7. `/ahome see <player>` from an admin.
8. The cursor jump needed the **host** fix above, not just the plugin. If it still
   jumps, the server binary predates the patch — `pumpkin-core/src/entity/player.rs`
   must have `on_handled_screen_closed()` where it used to have
   `close_handled_screen()` in `open_handled_screen_direct`.
9. Right-click a home's bed: the screen should **close by itself** and the prompt
   should appear. Then type a name. Bedrock players must press ESC themselves.

## Layout

| Module | Role |
|---|---|
| `text` | `%placeholder%` and legacy `&` colour |
| `homes` | the home record and slot-list rules |
| `layout` | both screen layouts |
| `config` | the seven files and their accessors |
| `store` | the JSON homes file |
| `state` | config, sessions, renames, combat, uuid lookups |
| `items` | Bukkit material names, GUI item building |
| `permissions` | `home.admin`, `advancedhomes.limit.N` |
| `homes_gui` | slot arithmetic, pagination, click routing |
| `confirm_gui` | the delete screen and its `nbt:` buttons |
| `teleport` | the countdown and its task registry |
| `sounds` | Bukkit sound names to resource locations |
| `messages` | prefix, placeholders, chat/actionbar gates |
| `screens` | building and opening the screens *(not host-tested)* |
| `commands` | the four commands |
| `lib` | wiring and the six event handlers |