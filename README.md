# ModernHome for Pumpkin

A Pumpkin Wasm port of [AdvancedHome](https://github.com/Rennex07/AdvancedHome). It provides home management through commands and inventory screens.

## Build

The plugin API is taken from the sibling `Pumpkin-release` checkout. Use the same WIT as the server; the current local checkout is Pumpkin 0.2.1.

```sh
cargo test
cargo build --locked --offline --release --target wasm32-wasip3
```

Install `target/wasm32-wasip3/release/pumpkin_home.wasm` in the server's plugin directory.

Use the Rust 1.100.0 toolchain pinned in `rust-toolchain.toml` and fetch dependencies
with `cargo fetch --locked` before an offline build. The server must support WASIp3
as well as the pinned plugin WIT.

When upgrading from PumpkinHome, stop the server and copy `plugins/data/pumpkinhome` to `plugins/data/ModernHome`. Update any plugin permission override to `ModernHome` and command-root grants to `ModernHome:home` or `ModernHome:ahome`.

## Usage

- `/home` opens the homes screen; `/sethome` saves a home; `/delhome <number>` deletes one.
- `/ahome reload`, `/ahome help`, `/ahome see <player>` and `/ahome osee <player>` are administrative commands.
- `home.admin` grants administrative access; `advancedhomes.limit.N` sets a home limit. The largest granted N wins, up to `home-limit-scan-max` in the configuration.
- Messages, GUI layouts and sounds are configurable in the plugin data directory. Defaults are written only when files are missing: updating the plugin does not overwrite existing configurations.
- The rename prompts are configured in `gui/homes.yml` using `home_rename` and `home_rename_actionbar`.

## Differences and testing

Homes use a JSON file rather than SQL. Redis cross-server transport, EssentialsX import and offline player lookup are not available. GUI behavior depends on Pumpkin's inventory API; the close-screen workaround is Java-client-specific. Existing GUI and message configurations may be reused, but defaults have been restyled.

New homes record both the world folder and dimension. Older home records are preserved; if their world name matches several loaded dimensions, teleporting is refused rather than guessing. Set those homes again in the correct dimension, or restore the correct dimension identifier from a trusted backup.

Home changes are saved through a temporary file and atomic replacement. If saving fails, the change is rolled back in memory and the player receives a failure message. Keep regular backups of `homes.json`.

The pure logic is unit-tested; GUI, teleport and event handling still require an in-game check. On a scratch server, test setting, renaming and deleting a home, teleport countdown and cancellation, admin permissions and a restart. Check the screens with a real client.

See [API-AUDIT.md](API-AUDIT.md) for the 0.2.1 API review and countdown fixes.
