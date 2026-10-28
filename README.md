# ModernHome for Pumpkin

A Pumpkin Wasm port of [AdvancedHome](https://github.com/Rennex07/AdvancedHome). It provides home management through commands and inventory screens.

## Build

The plugin API is taken from the sibling `Pumpkin-release` checkout. Use the same WIT as the server; the current local checkout is Pumpkin 0.2.1.

```sh
cargo test
cargo build --release --target wasm32-wasip2
```

Install `target/wasm32-wasip2/release/pumpkin_home.wasm` in the server's plugin directory.

## Usage

- `/home` opens the homes screen; `/sethome` saves a home; `/delhome <number>` deletes one.
- `/ahome reload`, `/ahome help`, `/ahome see <player>` and `/ahome osee <player>` are administrative commands.
- `home.admin` grants administrative access; `advancedhomes.limit.N` sets a home limit. The largest granted N wins, up to `home-limit-scan-max` in the configuration.
- Messages, GUI layouts and sounds are configurable in the plugin data directory. Defaults are written only when files are missing: updating the plugin does not overwrite existing configurations.

## Differences and testing

Homes use a JSON file rather than SQL. Redis cross-server transport, EssentialsX import and offline player lookup are not available. GUI behavior depends on Pumpkin's inventory API; the close-screen workaround is Java-client-specific. Existing GUI and message configurations may be reused, but defaults have been restyled.

The pure logic is unit-tested; GUI, teleport and event handling still require an in-game check. On a scratch server, test setting, renaming and deleting a home, teleport countdown and cancellation, admin permissions and a restart. Check the screens with a real client.
