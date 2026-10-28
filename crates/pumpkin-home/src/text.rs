//! Turning a configured string into something a vanilla client actually colours.
//!
//! The Java original had two steps in `ColorUtils.color`: translate `&` codes
//! with `ChatColor.translateAlternateColorCodes`, then rewrite every `#RRGGBB`
//! into the legacy `§x§R§R§G§G§B§B` form. Pumpkin has no equivalent of that
//! second step, because its legacy parser understands `#RRGGBB` directly, so the
//! second-byte muffin of `from_legacy_string_with_code` covers both. A template
//! written for the Paper plugin therefore renders the same colours here.
//!
//! ## Placeholders
//!
//! The Java original walked its placeholder `HashMap` and called
//! `String.replace` once per entry, so a *value* could be substituted again if
//! it happened to contain another key - `%index%` inside a home name, say.
//! [`apply`] scans left to right and substitutes every `%key%` it recognises in
//! one pass, which is what the Paper code intended. The two differ only when a
//! value itself contains a `%placeholder%`, and where they differ this one is
//! the answer you would expect.

/// The colour code the configuration is written in.
///
/// `&a`, `&l`, `&r`, `&#RRGGBB` and the spaced `&x&R&R&G&G&B&B` are all read by
/// the host's legacy parser (`pumpkin-util/src/text/legacy.rs`), so a template
/// carried over from Paper needs no rewriting.
pub const CODE: char = '&';

/// Substitutes every `%key%` in `input` in a single left-to-right pass.
///
/// A `%` that does not open a known key is left exactly as written, so a literal
/// `%player%` in a home's custom name survives a rename round-trip, and so does
/// the `%%` someone might use to escape one.
///
/// Values are owned rather than borrowed, because most of them are computed at
/// the call site - a page number, a count, a limit - and a borrowed form would
/// force every caller to hold its temporaries alive in order to name them.
pub fn apply(input: &str, placeholders: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(open) = rest.find('%') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];

        let Some(close) = after.find('%') else {
            // A lone `%` with no partner: nothing to substitute, so it is text.
            out.push('%');
            rest = after;
            continue;
        };

        let token = &after[..close];
        // The keys are written `%like%` because that is how a template spells
        // them, while the token the scan found is the bare name between two `%`.
        // Stripping the delimiters off the key is cheaper than rebuilding them
        // around the token once per candidate.
        match placeholders
            .iter()
            .find(|(name, _)| bare(name) == token)
        {
            Some((_, value)) => {
                out.push_str(value);
                rest = &after[close + 1..];
            }
            // An unknown key is left alone, including both of its `%`, which is
            // what stops a `%foo%` from being eaten by the scan.
            None => {
                out.push('%');
                out.push_str(&after[..=close]);
                rest = &after[close + 1..];
            }
        }
    }

    out.push_str(rest);
    out
}

/// A placeholder key without its `%` delimiters.
///
/// Accepts the key spelled either way, so a caller may pass `"%index%"` - which is
/// what a template contains - or `"index"`.
fn bare(name: &str) -> &str {
    name.strip_prefix('%')
        .unwrap_or(name)
        .strip_suffix('%')
        .unwrap_or_else(|| name.strip_prefix('%').unwrap_or(name))
}

/// Rewrites `#RRGGBB` into the legacy hex form the host's parser understands.
///
/// This is the second half of Paper's `ColorUtils.color`, and the half a plain
/// `&`-code pipeline is missing. `ColorUtils.color` did two things to a rendered
/// string: `translateAlternateColorCodes('&', …)`, which this crate gets from the
/// host's parser, and then a `Pattern.compile("#([A-Fa-f0-9]{6})")` pass that
/// rewrote each match as `§x` followed by six `§`-prefixed nibbles
/// (`ColorUtils.java:17-26`). It ran on the string *after* placeholders were
/// substituted (`HomesGuiConfig.java:168`), so a home named `&#FF0000` came out
/// red on the bed.
///
/// The host's parser already understands `&x&R&R&G&G&B&B` (`pumpkin-util/src/text/legacy.rs:62-99`),
/// so that is what is emitted here rather than reinventing the expansion.
///
/// A leading `&` is consumed along with the `#`. `&#FF0000` would otherwise become
/// `&&x…`, and that stray `&` is a code character to the parser - it would eat the
/// `x` and colour the text from there instead.
#[must_use]
pub fn legacy_hex(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;

    while i < bytes.len() {
        let hash = if bytes[i] == b'&' && bytes.get(i + 1) == Some(&b'#') {
            i + 1
        } else if bytes[i] == b'#' {
            i
        } else {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        };

        // `&#` is two bytes, but `ascii_hex_at` is indexed from the `#`.
        let Some(nibbles) = ascii_hex_at(bytes, hash + 1, 6) else {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        };

        out.push_str(&CODE.to_string());
        out.push('x');
        for nibble in nibbles {
            out.push(CODE);
            out.push(nibble);
        }
        i = hash + 7;
    }

    out
}

/// Six hex digits starting at `at`, as chars, or `None` if any is not a hex digit.
///
/// `None` rather than a partial match: `#FF00` is not a colour, and stopping halfway
/// would turn `#FF00GG` into a colour followed by `GG`.
fn ascii_hex_at(bytes: &[u8], at: usize, count: usize) -> Option<Vec<char>> {
    let slice = bytes.get(at..at + count)?;
    let mut out = Vec::with_capacity(count);
    for &b in slice {
        let c = b.to_ascii_lowercase() as char;
        if !c.is_ascii_hexdigit() {
            return None;
        }
        out.push(c);
    }
    Some(out)
}

/// The style MiniMessage tags build up, as the host's legacy parser wants it.
///
/// A plain stack of flags rather than a component tree: a close tag restores the
/// style underneath by re-emitting it after a reset, which is exactly what MiniMessage
/// nesting means and is a job the legacy parser has no way to do itself.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct Style {
    hex: Option<[u8; 6]>,
    colour: Option<char>,
    bold: bool,
    italic: bool,
    underlined: bool,
    strikethrough: bool,
    obfuscated: bool,
}

/// The sixteen named colours, and the legacy code each is written as.
const COLOURS: [(&str, char); 16] = [
    ("black", '0'),
    ("dark_blue", '1'),
    ("dark_green", '2'),
    ("dark_aqua", '3'),
    ("dark_red", '4'),
    ("dark_purple", '5'),
    ("gold", '6'),
    ("gray", '8'),
    ("dark_gray", '7'),
    ("blue", '9'),
    ("green", 'a'),
    ("aqua", 'b'),
    ("red", 'c'),
    ("light_purple", 'd'),
    ("yellow", 'e'),
    ("white", 'f'),
];

/// One entry of the tag stack: the style it introduced, and the tag that named it.
type Frame = (Style, String);

/// Rewrites the MiniMessage subset into the `&` codes the host's parser reads.
///
/// ## This is a departure, and a deliberate one
///
/// The original does not do this. `AdvancedHome` imports only
/// `LegacyComponentSerializer` and has no MiniMessage dependency, so `<red>Foo</red>`
/// reached the client as the literal text `<red>Foo</red>`. Rendering it as a colour
/// is an addition, not a port, and is here because a home name is the one place a
/// player types formatting themselves.
///
/// ## What is supported
///
/// Named colours, `<#RRGGBB>`, `<bold>`, `<italic>`, `<underlined>`,
/// `<strikethrough>`, `<obfuscated>`, `<reset>`, closing tags, nesting, and `\<` to
/// escape. `&` codes keep working throughout, including in the same string and the
/// same line, because that is what the templates themselves are written in.
///
/// ## What is not
///
/// Argument tags (`<color:#ff0000>`), the `_` decoration forms, `<hover>`,
/// `<click>`, `<insert>`, gradients and `<newline>`. These are left as literal text,
/// which is visible rather than silently wrong.
///
/// ## Why not build a component tree
///
/// `TextComponent` in the WIT has the builders a tree needs (`add-child`, `bold`,
/// `color-named`, ...), so it was possible - but it would mean replacing the legacy
/// path that every template already goes through. Rewriting to `&` codes keeps the
/// one path, and re-emitting a whole restored style after `&r` gives the nesting the
/// legacy codes alone cannot express.
#[must_use]
pub fn minimessage(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut stack: Vec<Frame> = Vec::new();
    let mut style = Style::default();
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            // `\<` is a literal `<`, and is the only escape.
            b'\\' if bytes.get(i + 1) == Some(&b'<') => {
                out.push('<');
                i += 2;
            }
            b'<' => {
                let Some((tag, closing, next)) = read_tag(bytes, i) else {
                    out.push('<');
                    i += 1;
                    continue;
                };
                match apply_tag(&tag, closing, &mut stack, &mut style) {
                    // A reset is only emitted when the style actually moved: an
                    // unchanged style would produce a redundant reset plus a full
                    // re-statement of what is already on.
                    Some(true) => write_style(&mut out, &style),
                    Some(false) => {}
                    None => {
                        // Not a tag this subset knows. Put it back as written rather
                        // than dropping it, so an unsupported tag is visible.
                        out.push_str(&input[i..next]);
                    }
                }
                i = next;
            }
            _ => {
                let start = i;
                while i < bytes.len() && bytes[i] != b'<' && bytes[i] != b'\\' {
                    i += 1;
                }
                out.push_str(&input[start..i]);
            }
        }
    }

    // Tags left open are not closed off: in MiniMessage an unclosed tag runs to the
    // end of the string, and that is the behaviour being reproduced.
    out
}

/// Reads a `<...>` tag at `at`.
///
/// Returns `(name, closing, end)` where `end` is just past the `>`, or `None` when
/// there is no `>` - which makes it text rather than a tag.
fn read_tag(bytes: &[u8], at: usize) -> Option<(String, bool, usize)> {
    let closing = bytes.get(at + 1) == Some(&b'/');
    let body = at + if closing { 2 } else { 1 };
    let end = bytes[body..].iter().position(|&b| b == b'>')? + body;
    let name = String::from_utf8_lossy(&bytes[body..end]).into_owned();
    Some((name, closing, end + 1))
}

/// Applies one tag, reporting whether the resulting style differs from the old one.
fn apply_tag(tag: &str, closing: bool, stack: &mut Vec<Frame>, style: &mut Style) -> Option<bool> {
    if closing {
        // Only a tag that is actually open can be closed, and only the innermost one
        // - so `</bold>` inside `<red>` leaves the red alone.
        if stack.last().is_some_and(|(_, name)| name == tag) {
            let (previous, _) = stack.pop().expect("checked just above");
            *style = previous;
            return Some(true);
        }
        // Nothing here to close. Reporting it as unknown rather than as a no-op is
        // what makes the closing half of an unsupported pair stay visible - `<hover>`
        // is not supported, so its `</hover>` must not quietly vanish and leave a
        // stray opening tag on screen.
        return None;
    }

    let mut next = *style;
    if tag == "reset" {
        next = Style::default();
    } else if let Some((_, code)) = COLOURS.iter().find(|(name, _)| *name == tag) {
        next.hex = None;
        next.colour = Some(*code);
    } else if let Some(hex) = tag.strip_prefix('#').and_then(|h| ascii_hex_at(h.as_bytes(), 0, 6)) {
        let mut bytes = [0u8; 6];
        for (slot, nibble) in bytes.iter_mut().zip(hex) {
            *slot = nibble as u8;
        }
        next.hex = Some(bytes);
        next.colour = None;
    } else {
        match tag {
            "bold" => next.bold = true,
            "italic" => next.italic = true,
            "underlined" => next.underlined = true,
            "strikethrough" => next.strikethrough = true,
            "obfuscated" => next.obfuscated = true,
            _ => return None,
        }
    }

    let changed = next != *style;
    stack.push((*style, tag.to_string()));
    *style = next;
    Some(changed)
}

/// Emits a reset followed by everything `style` turns on.
fn write_style(out: &mut String, style: &Style) {
    out.push(CODE);
    out.push('r');
    if let Some(hex) = style.hex {
        out.push(CODE);
        out.push('x');
        for nibble in hex {
            out.push(CODE);
            out.push(nibble as char);
        }
    } else if let Some(colour) = style.colour {
        out.push(CODE);
        out.push(colour);
    }
    for (on, code) in [
        (style.bold, 'l'),
        (style.italic, 'o'),
        (style.underlined, 'n'),
        (style.strikethrough, 'm'),
        (style.obfuscated, 'k'),
    ] {
        if on {
            out.push(CODE);
            out.push(code);
        }
    }
}

/// Everything a rendered string goes through before it reaches the host's parser:
/// MiniMessage tags, then hex.
///
/// Order matters and is Paper's: the tags and the hex are both rewriting a string
/// that already has its placeholders substituted in, so a home name's own formatting
/// is what gets read. `&` codes pass through both steps untouched.
#[must_use]
pub fn decorate(input: &str) -> String {
    legacy_hex(&minimessage(input))
}

/// Whether `c` is a character the legacy `&` codes use.
///
/// The same set `ChatColor.translateAlternateColorCodes` recognises: `0`-`9`,
/// `a`-`f`, `k`-`o` (the range, not three letters) and `r`. `x` is included because
/// [`legacy_hex`] emits `&x` followed by six `&`-prefixed nibbles.
const fn is_code_char(c: u8) -> bool {
    c.is_ascii_digit()
        || matches!(c, b'a'..=b'f' | b'k'..=b'o' | b'r' | b'x')
}

/// Splits a rendered string into `(is_code, text)` runs.
///
/// A `true` run is safe to hand to the host's legacy parser: it contains only
/// colour codes and ordinary text. A `false` run must not go near that parser,
/// because it contains a `&` that is not a code and the parser would eat it and
/// the character after it (`pumpkin-util/src/text/legacy.rs:58` takes the next
/// character as the code, `:131` discards it when it is unknown). Paper's
/// `translateAlternateColorCodes` leaves both alone, and this is what lets a
/// caller leave them alone too.
///
/// **A code and the text it applies to stay in the same run.** That is the whole
/// point of the split, and getting it wrong is silent: `&fClick to create a home`
/// cut into `["&f", "Click to create a home"]` parses the first run into an
/// *empty* component - the colour is set internally, but `remainder` is empty so
/// no styled child is ever emitted - and the second run is then added as plain
/// default-styled text. The lore appears, in the wrong colour, with nothing in
/// any log to say why.
///
/// So a run ends only at a `&` that is *not* a valid code. A literal run holds
/// that one character; everything after it resumes as ordinary text.
#[must_use]
pub fn segments(input: &str) -> Vec<(bool, String)> {
    let bytes = input.as_bytes();
    let mut runs: Vec<(bool, String)> = Vec::new();
    let mut coded = String::new();
    let mut i = 0;

    while i < bytes.len() {
        let is_bare_ampersand = bytes[i] == b'&'
            && !bytes.get(i + 1).is_some_and(|&c| is_code_char(c));

        if is_bare_ampersand {
            if !coded.is_empty() {
                runs.push((true, std::mem::take(&mut coded)));
            }
            runs.push((false, "&".to_string()));
            i += 1;
            continue;
        }

        if bytes[i] == b'&' {
            // A valid code. Both bytes stay in the coded run, and the text that
            // follows joins it so the parser can apply the style to it.
            coded.push(CODE);
            coded.push(bytes[i + 1] as char);
            i += 2;
            continue;
        }

        let width = utf8_width(bytes[i]);
        coded.push_str(&input[i..i + width]);
        i += width;
    }

    if !coded.is_empty() {
        runs.push((true, coded));
    }
    runs
}

/// How many bytes the character starting at `first` occupies.
///
/// Iterating bytes and slicing on byte boundaries is only correct if the width is
/// known, and the width is not derivable from the leading byte alone in general.
/// Every code and space that reaches here is ASCII, but a home name may hold any
/// UTF-8, so this reads the continuation bytes rather than assuming.
fn utf8_width(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Strips the legacy `§` colour codes the Java rename path stripped.
///
/// Paper did this with `replaceAll("§[0-9a-fk-or]", "")` on a chat message it
/// had already round-tripped through `LegacyComponentSerializer`. The character
/// class is `0-9`, `a-f`, the range `k-o` (obfuscated, bold, strikethrough,
/// underlined, italic) and a bare `r` - which is all twenty-two of the codes the
/// game actually has. `k-o` being a range rather than three letters is easy to
/// misread, so it is spelled out in [`is_stripped_code`] rather than left in a
/// regex.
///
/// `&` codes are deliberately **not** stripped, and were not there either: they
/// are what the templates are written in, so removing them would change what the
/// GUI shows rather than what is stored.
pub fn strip_legacy_codes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();

    while let Some(c) = chars.next() {
        if c == '\u{00a7}' {
            // Consume the code character too, whatever it is. An unmatched `§`
            // at the end is dropped rather than left dangling.
            if let Some(code) = chars.next() {
                if is_stripped_code(code) {
                    continue;
                }
                out.push('\u{00a7}');
                out.push(code);
                continue;
            }
            continue;
        }
        out.push(c);
    }

    out
}

/// Whether `code` is one of the codes [`strip_legacy_codes`] removes.
///
/// `k-o` is a range, which is what makes the Paper pattern strip bold,
/// strikethrough and underlined as well as obfuscated and italic. Spelled as a
/// `matches!` so the range survives being read.
fn is_stripped_code(code: char) -> bool {
    code.is_ascii_digit() || matches!(code, 'a'..='f' | 'k'..='r')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two placeholders most tests need.
    ///
    /// A function rather than a `const`, because a `String` is not const-built and
    /// the list is small enough that making one per call is free.
    fn placeholders() -> [(&'static str, String); 2] {
        [
            ("%index%", "3".to_string()),
            ("%home%", "base".to_string()),
        ]
    }

    #[test]
    fn a_value_is_substituted_once_and_never_re_scanned() {
        // The Paper behaviour was the opposite: it walked its placeholder map
        // calling `String::replace` once per entry, so a *value* was substituted
        // again if it happened to contain another key - `%index%` inside a home
        // name, say - giving `&9Home 3 3` here.
        //
        // This one is a single left-to-right pass, so the substituted value is
        // written out as-is and the `%index%` inside it stays text. That is the
        // difference stated rather than hidden, and it is the one place the port
        // does not copy.
        let placeholders = [
            ("%home%", "%index%".to_string()),
            ("%index%", "3".to_string()),
        ];
        assert_eq!(apply("&9Home %index% %home%", &placeholders), "&9Home 3 %index%");
    }

    #[test]
    fn an_unknown_key_is_left_exactly_as_written() {
        assert_eq!(
            apply("%foo% and %bar%", &placeholders()),
            "%foo% and %bar%"
        );
    }

    #[test]
    fn a_lone_percent_is_text_not_a_scan() {
        assert_eq!(apply("100% done", &placeholders()), "100% done");
        assert_eq!(apply("a % b", &placeholders()), "a % b");
    }

    #[test]
    fn a_home_name_holding_a_key_survives_the_gui_round_trip() {
        // Stored name `%player%` must not be rewritten when the item name that
        // interpolates it is rendered, or the name would change on screen.
        let placeholders = [("%home%", "%player%".to_string())];
        assert_eq!(apply("&9Home 1 %home%", &placeholders), "&9Home 1 %player%");
    }

    #[test]
    fn an_empty_value_substitutes_to_nothing() {
        // `%home%` is the empty string when a home has no custom name, and the
        // configured template then ends in a space. That trailing space is the
        // Paper plugin's own output and is reproduced rather than trimmed.
        assert_eq!(
            apply(
                "&9Home %index% %home%",
                &[("%index%", "1".to_string()), ("%home%", String::new())]
            ),
            "&9Home 1 "
        );
    }

    #[test]
    fn a_hex_colour_becomes_the_legacy_form_the_host_parser_reads() {
        // `ColorUtils.java:17-26` ran a `#([A-Fa-f0-9]{6})` pass over the rendered
        // string and emitted `§x§R§R§G§G§B§B`, which is what the host's parser
        // understands at `pumpkin-util/src/text/legacy.rs:62-99`.
        assert_eq!(legacy_hex("#FF0000"), "&x&f&f&0&0&0&0");
        assert_eq!(legacy_hex("&#1A2B3C"), "&x&1&a&2&b&3&c");
        // Mixed case is folded, matching `ChatColor`'s nibbles being lowercased
        // into the same parser.
        assert_eq!(legacy_hex("#AbCdEf"), "&x&a&b&c&d&e&f");
    }

    #[test]
    fn a_hash_that_is_not_a_colour_is_left_alone() {
        // A `#` is an ordinary character in a home name, and a short one is not a
        // colour. Turning `#FF00` into a colour would silently eat text.
        assert_eq!(legacy_hex("#FF00"), "#FF00");
        assert_eq!(legacy_hex("#GGGGGG"), "#GGGGGG");
        assert_eq!(legacy_hex("issue #12345 here"), "issue #12345 here");
        // Nothing to do at all.
        assert_eq!(legacy_hex("&9Home 1 &cRed"), "&9Home 1 &cRed");
        assert_eq!(legacy_hex(""), "");
    }

    #[test]
    fn a_colour_in_a_home_name_reaches_the_template_it_is_substituted_into() {
        // The order is Paper's: placeholders first (`HomesGuiConfig.java:168`), so a
        // home named `&#FF0000` colours its own bed rather than printing the code.
        let rendered = apply(
            "&9Home %index% %home%",
            &[
                ("%index%", "1".to_string()),
                ("%home%", "&#FF0000".to_string()),
            ],
        );
        assert_eq!(rendered, "&9Home 1 &#FF0000");
        assert_eq!(legacy_hex(&rendered), "&9Home 1 &x&f&f&0&0&0&0");
    }

    #[test]
    fn a_minimessage_tag_becomes_the_legacy_code_the_host_reads() {
        // The point of the subset: a player naming a home `<red>Base</red>` gets a red
        // name. The original rendered this as the literal text, so this is an addition.
        assert_eq!(minimessage("<red>Base</red>"), "&r&cBase&r");
        assert_eq!(minimessage("<bold>Hi</bold>"), "&r&lHi&r");
        // A reset with nothing styled is a no-op, so it costs no bytes on the wire.
        assert_eq!(minimessage("<reset>x"), "x");
        assert_eq!(minimessage("<red>a<reset>b"), "&r&ca&rb");
        // Every named colour maps to the code the game uses for it.
        for (name, code) in COLOURS {
            assert_eq!(
                minimessage(&format!("<{name}>x</{name}>")),
                format!("&r&{code}x&r"),
                "{name} is &{code}"
            );
        }
    }

    #[test]
    fn closing_a_tag_restores_the_style_underneath_it() {
        // This is what the legacy codes cannot do on their own, and the reason the
        // whole style is re-emitted after the reset rather than just stopping.
        // `c` comes back red: the close re-states the style underneath, which is the whole
        // reason a close emits a reset plus the restored style rather than nothing.
        assert_eq!(minimessage("<red>a<green>b</green>c</red>"), "&r&ca&r&ab&r&cc&r");
        // Decorations nest inside colours and come back off. Each tag emits a reset plus
        // the *whole* current style rather than only what changed, so the colour is
        // stated twice here - redundant on the wire, identical on screen, and much
        // easier to get right than tracking deltas.
        assert_eq!(
            minimessage("<red><bold>a</bold>b</red>"),
            "&r&c&r&c&la&r&cb&r"
        );
        // A close that does not match what is open is left as written rather than
        // unwinding something the player did not ask to unwind - the same rule that
        // keeps an unsupported tag visible.
        assert_eq!(minimessage("<red><bold>a</red>"), "&r&c&r&c&la</red>");
    }

    #[test]
    fn a_minimessage_hex_tag_matches_the_legacy_hex_syntax() {
        assert_eq!(minimessage("<#FF0000>x"), "&r&x&f&f&0&0&0&0x");
        // A colour inside a colour replaces it rather than stacking.
        assert_eq!(minimessage("<red><#00FF00>x"), "&r&c&r&x&0&0&f&f&0&0x");
    }

    #[test]
    fn a_tag_this_subset_does_not_know_is_left_visible() {
        // Unsupported syntax stays as written. Rendering it as a colour would be a
        // silent lie about what the plugin supports.
        assert_eq!(minimessage("<hover>x</hover>"), "<hover>x</hover>");
        assert_eq!(minimessage("<gradient:#a>#b</gradient>"), "<gradient:#a>#b</gradient>");
        // An unmatched `<` is text.
        assert_eq!(minimessage("3 < 5"), "3 < 5");
        assert_eq!(minimessage("a < b > c"), "a < b > c");
    }

    #[test]
    fn a_escaped_tag_is_literal_text() {
        assert_eq!(minimessage("\\<red>Base"), "<red>Base");
    }

    #[test]
    fn legacy_codes_keep_working_alongside_minimessage() {
        // Every template in the shipped config is legacy-coded, so the two must mix
        // in one string without either disturbing the other.
        assert_eq!(decorate("&9Home 1 <red>Base</red>"), "&9Home 1 &r&cBase&r");
        // And a home name may be MiniMessage, legacy hex, or either.
        assert_eq!(decorate("&#FF0000"), "&x&f&f&0&0&0&0");
        assert_eq!(decorate("<#FF0000>"), "&r&x&f&f&0&0&0&0");
        assert_eq!(decorate("&cRed"), "&cRed");
    }

    #[test]
    fn a_home_name_reaches_its_own_bed_styled() {
        // The whole path, in the order Paper applied it: placeholders, then colour.
        let rendered = apply(
            "&9Home %index% %home%",
            &[("%index%", "1".to_string()), ("%home%", "<red>Base</red>".to_string())],
        );
        assert_eq!(rendered, "&9Home 1 <red>Base</red>");
        assert_eq!(decorate(&rendered), "&9Home 1 &r&cBase&r");
    }

    #[test]
    fn an_ampersand_that_is_not_a_colour_is_kept_out_of_the_parser() {
        // The host's parser takes the character after every `&` as the code and then
        // throws it away if it is not one (`legacy.rs:58` and `:131`), so these have
        // to be split off before it sees them.
        assert_eq!(segments("Fish & Chips"), vec![(true, "Fish ".to_string()), (false, "&".to_string()), (true, " Chips".to_string())]);
        // `H` is not a colour, which is the case from the bug report.
        assert_eq!(segments("&HI"), vec![(false, "&".to_string()), (true, "HI".to_string())]);
        assert!(segments("").is_empty());
    }

    #[test]
    fn a_colour_code_stays_with_the_text_it_colours() {
        // The bug this guards: splitting `&fClick` into `["&f", "Click"]` makes the
        // parser produce an *empty* component for `&f` - the colour is set internally
        // but nothing is emitted - so the text lands with default styling. That
        // rendered the lore in the wrong colour with nothing in any log.
        assert_eq!(
            segments("&fClick to create a home"),
            vec![(true, "&fClick to create a home".to_string())],
            "a code and its text are one run, so the parser can style the text"
        );
        // Every run a pure-code template produces stays a single run, which is what
        // keeps the ordinary path on the host's parser in one call.
        assert_eq!(
            segments("&9Home 1"),
            vec![(true, "&9Home 1".to_string())]
        );
        assert!(segments("&9Home 1").iter().all(|(is_code, _)| *is_code));
        // A code in the middle still stays with what precedes and follows it.
        assert_eq!(
            segments("&9Home &cred&r"),
            vec![(true, "&9Home &cred&r".to_string())]
        );
    }

    #[test]
    fn colour_codes_the_paper_templates_use_are_left_alone() {
        // Colour parsing is the host's job, and `&9Home` reaching it intact is
        // what makes the shipped screens look the same.
        assert_eq!(apply("&9Home %index%", &placeholders()), "&9Home 3");
        assert_eq!(
            apply("&#FF0000red %index%", &placeholders()),
            "&#FF0000red 3"
        );
    }

    #[test]
    fn the_legacy_codes_the_paper_rename_stripped_are_removed() {
        assert_eq!(strip_legacy_codes("\u{00a7}c\u{00a7}lred"), "red");
        assert_eq!(strip_legacy_codes("\u{00a7}0plain"), "plain");
        assert_eq!(strip_legacy_codes("\u{00a7}oitalic"), "italic");
    }

    #[test]
    fn every_minecraft_code_range_is_covered_not_just_three_letters() {
        // The Paper pattern is `[0-9a-fk-or]`, and `k-o` is a *range*: it takes
        // bold, strikethrough and underlined with it, plus the bare `r`. Reading
        // it as `k`, `o`, `r` would leave those codes in stored names, which is
        // the bug this test exists to prevent.
        for code in ['k', 'l', 'm', 'n', 'o', 'r'] {
            let name = format!("a\u{00a7}{code}b");
            assert_eq!(
                strip_legacy_codes(&name),
                "ab",
                "§{code} was not stripped, but Paper's [0-9a-fk-or] strips it"
            );
        }
        for code in 'a'..='z' {
            let expected = if code.is_ascii_digit()
                || matches!(code, 'a'..='f' | 'k'..='r')
            {
                "ab".to_string()
            } else {
                format!("a\u{00a7}{code}b")
            };
            let name = format!("a\u{00a7}{code}b");
            assert_eq!(strip_legacy_codes(&name), expected, "§{code} disagrees");
        }
    }

    #[test]
    fn an_ampersand_code_is_not_stripped_from_a_typed_name() {
        // A player who typed `&cred` in Paper stored `&cred`; the GUI coloured
        // it because the item name is parsed for `&` codes. Same here.
        assert_eq!(strip_legacy_codes("&cred"), "&cred");
    }

    #[test]
    fn a_trailing_section_sign_is_dropped_rather_than_left_dangling() {
        assert_eq!(strip_legacy_codes("name\u{00a7}"), "name");
    }

    #[test]
    fn a_section_sign_before_a_letter_that_is_not_a_code_is_kept() {
        // Only a real code is removed; `§z` is not one, so both characters
        // survive, which is what a regex of `[0-9a-fk-or]` would also do.
        assert_eq!(strip_legacy_codes("\u{00a7}z"), "\u{00a7}z");
    }
}
