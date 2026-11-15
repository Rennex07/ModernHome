# ModernHome for Pumpkin

A Pumpkin Wasm port of [AdvancedHome](https://github.com/Rennex07/AdvancedHome). It provides home management through commands and inventory screens.

## Build

The plugin targets the **v0.2 plugin API** and is built for **WASIp3**. The v0.2
API is the one that runs on a Pumpkin built with the `plugin-v0_2` feature, and
WASIp3 is the WASI version that API's host links.

The v0.2 SDK is not published yet, so the crate pins it to the exact upstream
commit that carries it (upstream PR #3962). A component only loads on a server
whose WIT matches the one it was built against, so the pin is not a preference.

```sh
cargo test
cargo build --locked --offline --release --target wasm32-wasip3
```

Install `target/wasm32-wasip3/release/pumpkin_home.wasm` in the server's plugin directory.

Use the Rust 1.100.0 toolchain pinned in `rust-toolchain.toml` and fetch dependencies
with `cargo fetch --locked` before an offline build.

A server built from released Pumpkin cannot load this plugin: the released
builds leave the v0.2 API off, and the v0.1 API only accepts WASIp2. See
[API-AUDIT.md](API-AUDIT.md).

### The SQLite backend

The SQLite backend is a Cargo feature, because SQLite is compiled from C and that
needs a C compiler for the target:

```sh
cargo build --locked --offline --release --target wasm32-wasip3 --features sqlite
```

For a wasm target that compiler is the [wasi-sdk](https://github.com/WebAssembly/wasi-sdk)
clang, and `cc` has to be told where it is:

```sh
export CC_wasm32_wasip3="$WASI_SDK/bin/wasm32-wasip3-clang"
export AR_wasm32_wasip3="$WASI_SDK/bin/llvm-ar"
cargo build --locked --offline --release --target wasm32-wasip3 --features sqlite
```

Without the feature the plugin keeps the JSON backend only, and a `database.yml`
that asks for SQLite is reported rather than silently ignored. The released
artifact is built with the feature, so an installed plugin has both backends.

When upgrading from PumpkinHome, stop the server and copy `plugins/data/pumpkinhome` to `plugins/data/ModernHome`. Update any plugin permission override to `ModernHome`.

## Usage

- `/home` opens the homes screen; `/sethome` saves a home; `/delhome <number>` deletes one.
- `/ahome reload`, `/ahome help`, `/ahome see <player>` and `/ahome osee <player>` are administrative commands.
- `ModernHome:home` allows home commands and defaults to allowed.
- `ModernHome:admin` allows `/ahome`, administrative actions, unlimited homes and instant teleports. It defaults to operator level 2; ordinary players are not shown `/ahome` in Pumpkin's command tree. Explicit permission denials are respected.
- `ModernHome:limit.N` sets a home limit, for example `ModernHome:limit.10`. The largest granted N wins, up to `home-limit-scan-max` in the configuration. Non-admins without a limit grant get one home.
- Permission names are case-sensitive. Replace legacy `home.admin` grants with `ModernHome:admin` and `advancedhomes.limit.N` grants with `ModernHome:limit.N`. The former `ModernHome:ahome` root grant is replaced by `ModernHome:admin`.
- Messages, GUI layouts and sounds are configurable in the plugin data directory. Defaults are written only when files are missing: updating the plugin does not overwrite existing configurations.
- The rename prompts are configured in `gui/homes.yml` using `home_rename` and `home_rename_actionbar`.

## Differences and testing

Homes are kept in a JSON file by default, or in a SQLite database when `database.yml` says `storage: sqlite`. A remote MySQL or MariaDB server is not offered: there is no MySQL client that can be linked into a WebAssembly component for the host this plugin runs on. Redis cross-server transport, EssentialsX import and offline player lookup are not available. GUI behavior depends on Pumpkin's inventory API; the close-screen workaround is Java-client-specific. Existing GUI and message configurations may be reused, but defaults have been restyled.

New homes record both the world folder and dimension. Older home records are preserved; if their world name matches several loaded dimensions, teleporting is refused rather than guessing. Set those homes again in the correct dimension, or restore the correct dimension identifier from a trusted backup.

Home changes are saved through a temporary file and atomic replacement. If saving fails, the change is rolled back in memory and the player receives a failure message. Keep regular backups of `homes.json`.

The JSON and SQLite backends are two different files. Switching between them does not move the homes, and the file that is left behind is left exactly as it was.

The pure logic is unit-tested; GUI, teleport and event handling still require an in-game check. On a scratch server, test setting, renaming and deleting a home, teleport countdown and cancellation, admin permissions and a restart. Check the screens with a real client.

See [API-AUDIT.md](API-AUDIT.md) for the 0.2.1 API review and countdown fixes.
