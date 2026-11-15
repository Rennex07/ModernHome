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

Without this feature, selecting SQLite fails to load rather than falling back to JSON.
Use `--features sqlite,mysql` to build all storage backends.

### MySQL and MariaDB (experimental)

Build with `--features mysql` and set `storage: mysql` or `storage: mariadb`
in `database.yml`. Both names use the `mysql` configuration section.
Create the database and a dedicated account first; the plugin creates its table.
The host must grant `network.tcp.connect` and `network.dns`.

The client supports `mysql_native_password` and the cached fast path of
`caching_sha2_password`. Full SHA2 authentication, native TLS and other
authentication plugins are not implemented. Queries and results are unencrypted;
use a trusted network or an authenticated TLS tunnel. A server requiring
unsupported authentication is refused, not downgraded.

MariaDB 11.8.3 has been exercised through WASIp3 in Wasmtime 50. MySQL itself
and database access inside a Pumpkin callback remain unverified. This backend
is not ready for a production release.

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

Homes use JSON by default, with optional SQLite and experimental MySQL/MariaDB
backends. Redis cross-server transport, EssentialsX import and offline player
lookup are not available. GUI behavior depends on Pumpkin's inventory API;
the close-screen workaround is Java-client-specific.

New homes record both the world folder and dimension. Older home records are preserved; if their world name matches several loaded dimensions, teleporting is refused rather than guessing. Set those homes again in the correct dimension, or restore the correct dimension identifier from a trusted backup.

JSON saves use atomic file replacement; SQL saves use transactions. Failed
changes are rolled back in memory and reported to the player. A MySQL/MariaDB
save error disables further writes until restart because a disconnected COMMIT
may have an unknown outcome. Keep regular backups.

The JSON and SQLite backends are two different files. Switching between them does not move the homes, and the file that is left behind is left exactly as it was.

Storage settings take effect after a restart. Each backend is intended for one
plugin instance; the in-memory cache is not refreshed from other servers.
The WASI SQLite VFS does not provide database file locking.

### Database regression test

The integration test requires a disposable database and an account that can
create tables and triggers. Set `MODERNHOME_TEST_MYSQL_HOST`, `PORT`, `USER`,
`PASSWORD`, and `DATABASE` (all with the same prefix), then run:

```sh
cargo test --features mysql real_database_round_trip_and_rollback -- --ignored
```

The pure logic is unit-tested; GUI, teleport and event handling still require an in-game check. On a scratch server, test setting, renaming and deleting a home, teleport countdown and cancellation, admin permissions and a restart. Check the screens with a real client.

See [API-AUDIT.md](API-AUDIT.md) for the 0.2.1 API review and countdown fixes.
