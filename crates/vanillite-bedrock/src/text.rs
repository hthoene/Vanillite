//! Java text components as Bedrock text: translatable parts filled in from the server's English
//! language table, colours and formats as `§` codes that Bedrock understands.

use std::collections::HashMap;

use vanillite_protocol::text::Component;

/// The language table of the Java server (`en_us`; empty without the Vanilla pack: keys are
/// shown then).
pub type Language = HashMap<String, String>;

/// The Java legacy colours `§0`-`§f` as RGB, to map hex colours onto them.
const COLOURS: [(u8, u32); 16] = [
    (b'0', 0x000000),
    (b'1', 0x0000AA),
    (b'2', 0x00AA00),
    (b'3', 0x00AAAA),
    (b'4', 0xAA0000),
    (b'5', 0xAA00AA),
    (b'6', 0xFFAA00),
    (b'7', 0xAAAAAA),
    (b'8', 0x555555),
    (b'9', 0x5555FF),
    (b'a', 0x55FF55),
    (b'b', 0x55FFFF),
    (b'c', 0xFF5555),
    (b'd', 0xFF55FF),
    (b'e', 0xFFFF55),
    (b'f', 0xFFFFFF),
];

/// The Bedrock text of a component.
pub fn render(c: &Component, language: &Language) -> String {
    bedrock_codes(&c.to_legacy_in(&|key| language.get(key).cloned()))
}

/// Java legacy codes as Bedrock codes: Bedrock has no strikethrough or underline (`§m` and `§n`
/// are colours there) and no hex colours (`§x§r§r§g§g§b§b` becomes the nearest legacy colour).
fn bedrock_codes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '§' {
            out.push(c);
            continue;
        }
        let Some(code) = chars.next() else {
            break;
        };
        match code.to_ascii_lowercase() {
            'm' | 'n' => {}
            'x' => {
                let mut rgb = 0u32;
                for _ in 0..6 {
                    if chars.next_if_eq(&'§').is_none() {
                        break;
                    }
                    let digit = chars.next().and_then(|d| d.to_digit(16)).unwrap_or(0);
                    rgb = rgb << 4 | digit;
                }
                out.push('§');
                out.push(char::from(nearest(rgb)));
            }
            code => {
                out.push('§');
                out.push(code);
            }
        }
    }
    out
}

/// The legacy colour code nearest to `rgb`.
fn nearest(rgb: u32) -> u8 {
    let palette = COLOURS.map(|(_, c)| c);
    let i = crate::num::nearest_colour(rgb, &palette);
    COLOURS.get(i).map_or(b'f', |(code, _)| *code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_and_keeps_bedrock_codes() {
        let mut language = Language::new();
        language.insert(
            "multiplayer.player.joined".into(),
            "%s joined the game".into(),
        );
        let c =
            Component::translatable("multiplayer.player.joined", vec![Component::text(".Steve")]);
        assert_eq!(
            render(&c, &language).replace("§r", ""),
            ".Steve joined the game"
        );
        assert_eq!(bedrock_codes("§x§f§f§5§5§5§5a§mb§nc§ld"), "§cabc§ld");
    }
}
