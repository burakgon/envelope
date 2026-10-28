//! RFC 7986 `COLOR` support: CSS3 color names ↔ `#rrggbb`.

/// The CSS3 extended color keywords (plus `rebeccapurple`), as RFC 7986
/// requires for `COLOR`.
static CSS_COLORS: &[(&str, &str)] = &[
    ("aliceblue", "#f0f8ff"),
    ("antiquewhite", "#faebd7"),
    ("aqua", "#00ffff"),
    ("aquamarine", "#7fffd4"),
    ("azure", "#f0ffff"),
    ("beige", "#f5f5dc"),
    ("bisque", "#ffe4c4"),
    ("black", "#000000"),
    ("blanchedalmond", "#ffebcd"),
    ("blue", "#0000ff"),
    ("blueviolet", "#8a2be2"),
    ("brown", "#a52a2a"),
    ("burlywood", "#deb887"),
    ("cadetblue", "#5f9ea0"),
    ("chartreuse", "#7fff00"),
    ("chocolate", "#d2691e"),
    ("coral", "#ff7f50"),
    ("cornflowerblue", "#6495ed"),
    ("cornsilk", "#fff8dc"),
    ("crimson", "#dc143c"),
    ("cyan", "#00ffff"),
    ("darkblue", "#00008b"),
    ("darkcyan", "#008b8b"),
    ("darkgoldenrod", "#b8860b"),
    ("darkgray", "#a9a9a9"),
    ("darkgreen", "#006400"),
    ("darkgrey", "#a9a9a9"),
    ("darkkhaki", "#bdb76b"),
    ("darkmagenta", "#8b008b"),
    ("darkolivegreen", "#556b2f"),
    ("darkorange", "#ff8c00"),
    ("darkorchid", "#9932cc"),
    ("darkred", "#8b0000"),
    ("darksalmon", "#e9967a"),
    ("darkseagreen", "#8fbc8f"),
    ("darkslateblue", "#483d8b"),
    ("darkslategray", "#2f4f4f"),
    ("darkslategrey", "#2f4f4f"),
    ("darkturquoise", "#00ced1"),
    ("darkviolet", "#9400d3"),
    ("deeppink", "#ff1493"),
    ("deepskyblue", "#00bfff"),
    ("dimgray", "#696969"),
    ("dimgrey", "#696969"),
    ("dodgerblue", "#1e90ff"),
    ("firebrick", "#b22222"),
    ("floralwhite", "#fffaf0"),
    ("forestgreen", "#228b22"),
    ("fuchsia", "#ff00ff"),
    ("gainsboro", "#dcdcdc"),
    ("ghostwhite", "#f8f8ff"),
    ("gold", "#ffd700"),
    ("goldenrod", "#daa520"),
    ("gray", "#808080"),
    ("green", "#008000"),
    ("greenyellow", "#adff2f"),
    ("grey", "#808080"),
    ("honeydew", "#f0fff0"),
    ("hotpink", "#ff69b4"),
    ("indianred", "#cd5c5c"),
    ("indigo", "#4b0082"),
    ("ivory", "#fffff0"),
    ("khaki", "#f0e68c"),
    ("lavender", "#e6e6fa"),
    ("lavenderblush", "#fff0f5"),
    ("lawngreen", "#7cfc00"),
    ("lemonchiffon", "#fffacd"),
    ("lightblue", "#add8e6"),
    ("lightcoral", "#f08080"),
    ("lightcyan", "#e0ffff"),
    ("lightgoldenrodyellow", "#fafad2"),
    ("lightgray", "#d3d3d3"),
    ("lightgreen", "#90ee90"),
    ("lightgrey", "#d3d3d3"),
    ("lightpink", "#ffb6c1"),
    ("lightsalmon", "#ffa07a"),
    ("lightseagreen", "#20b2aa"),
    ("lightskyblue", "#87cefa"),
    ("lightslategray", "#778899"),
    ("lightslategrey", "#778899"),
    ("lightsteelblue", "#b0c4de"),
    ("lightyellow", "#ffffe0"),
    ("lime", "#00ff00"),
    ("limegreen", "#32cd32"),
    ("linen", "#faf0e6"),
    ("magenta", "#ff00ff"),
    ("maroon", "#800000"),
    ("mediumaquamarine", "#66cdaa"),
    ("mediumblue", "#0000cd"),
    ("mediumorchid", "#ba55d3"),
    ("mediumpurple", "#9370db"),
    ("mediumseagreen", "#3cb371"),
    ("mediumslateblue", "#7b68ee"),
    ("mediumspringgreen", "#00fa9a"),
    ("mediumturquoise", "#48d1cc"),
    ("mediumvioletred", "#c71585"),
    ("midnightblue", "#191970"),
    ("mintcream", "#f5fffa"),
    ("mistyrose", "#ffe4e1"),
    ("moccasin", "#ffe4b5"),
    ("navajowhite", "#ffdead"),
    ("navy", "#000080"),
    ("oldlace", "#fdf5e6"),
    ("olive", "#808000"),
    ("olivedrab", "#6b8e23"),
    ("orange", "#ffa500"),
    ("orangered", "#ff4500"),
    ("orchid", "#da70d6"),
    ("palegoldenrod", "#eee8aa"),
    ("palegreen", "#98fb98"),
    ("paleturquoise", "#afeeee"),
    ("palevioletred", "#db7093"),
    ("papayawhip", "#ffefd5"),
    ("peachpuff", "#ffdab9"),
    ("peru", "#cd853f"),
    ("pink", "#ffc0cb"),
    ("plum", "#dda0dd"),
    ("powderblue", "#b0e0e6"),
    ("purple", "#800080"),
    ("rebeccapurple", "#663399"),
    ("red", "#ff0000"),
    ("rosybrown", "#bc8f8f"),
    ("royalblue", "#4169e1"),
    ("saddlebrown", "#8b4513"),
    ("salmon", "#fa8072"),
    ("sandybrown", "#f4a460"),
    ("seagreen", "#2e8b57"),
    ("seashell", "#fff5ee"),
    ("sienna", "#a0522d"),
    ("silver", "#c0c0c0"),
    ("skyblue", "#87ceeb"),
    ("slateblue", "#6a5acd"),
    ("slategray", "#708090"),
    ("slategrey", "#708090"),
    ("snow", "#fffafa"),
    ("springgreen", "#00ff7f"),
    ("steelblue", "#4682b4"),
    ("tan", "#d2b48c"),
    ("teal", "#008080"),
    ("thistle", "#d8bfd8"),
    ("tomato", "#ff6347"),
    ("turquoise", "#40e0d0"),
    ("violet", "#ee82ee"),
    ("wheat", "#f5deb3"),
    ("white", "#ffffff"),
    ("whitesmoke", "#f5f5f5"),
    ("yellow", "#ffff00"),
    ("yellowgreen", "#9acd32"),
];

/// Normalizes a color value to lower-case `#rrggbb`.
///
/// Accepts CSS3 color names (any case), `#rgb`, `#rrggbb` and `#rrggbbaa`
/// (alpha is dropped), with or without the `#`.
///
/// ```
/// use envelope_ical::color::css_color_to_hex;
/// assert_eq!(css_color_to_hex("CornflowerBlue").as_deref(), Some("#6495ed"));
/// assert_eq!(css_color_to_hex("#F0A").as_deref(), Some("#ff00aa"));
/// ```
pub fn css_color_to_hex(value: &str) -> Option<String> {
    let value = value.trim().trim_matches('"').trim();
    let lower = value.to_ascii_lowercase();
    if let Some((_, hex)) = CSS_COLORS.iter().find(|(name, _)| *name == lower) {
        return Some((*hex).to_owned());
    }
    let digits = lower.strip_prefix('#').unwrap_or(&lower);
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    match digits.len() {
        3 | 4 => {
            let mut out = String::from("#");
            for c in digits.chars().take(3) {
                out.push(c);
                out.push(c);
            }
            Some(out)
        }
        6 | 8 => Some(format!("#{}", &digits[..6])),
        _ => None,
    }
}

/// The CSS3 color name for a `#rrggbb` value: an exact match if there is
/// one, otherwise the nearest named color.
pub fn hex_to_css_color_name(hex: &str) -> Option<&'static str> {
    let hex = css_color_to_hex(hex)?;
    if let Some((name, _)) = CSS_COLORS.iter().find(|(_, h)| *h == hex) {
        return Some(name);
    }
    let target = rgb(&hex)?;
    CSS_COLORS
        .iter()
        .filter_map(|(name, h)| rgb(h).map(|c| (*name, distance(c, target))))
        .min_by_key(|(_, d)| *d)
        .map(|(name, _)| name)
}

fn rgb(hex: &str) -> Option<(i32, i32, i32)> {
    let digits = hex.strip_prefix('#')?;
    let channel = |range: std::ops::Range<usize>| {
        digits
            .get(range)
            .and_then(|s| i32::from_str_radix(s, 16).ok())
    };
    Some((channel(0..2)?, channel(2..4)?, channel(4..6)?))
}

fn distance(a: (i32, i32, i32), b: (i32, i32, i32)) -> i32 {
    // "Redmean" weighting approximates perceived distance cheaply.
    let mean_red = (a.0 + b.0) / 2;
    let (dr, dg, db) = (a.0 - b.0, a.1 - b.1, a.2 - b.2);
    (((512 + mean_red) * dr * dr) >> 8) + 4 * dg * dg + (((767 - mean_red) * db * db) >> 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_hex_forms() {
        assert_eq!(CSS_COLORS.len(), 148);
        assert_eq!(css_color_to_hex("turquoise").as_deref(), Some("#40e0d0"));
        assert_eq!(css_color_to_hex(" Red ").as_deref(), Some("#ff0000"));
        assert_eq!(css_color_to_hex("#ABCDEF").as_deref(), Some("#abcdef"));
        assert_eq!(css_color_to_hex("abcdef80").as_deref(), Some("#abcdef"));
        assert_eq!(css_color_to_hex("#12").as_deref(), None);
        assert_eq!(css_color_to_hex("notacolor"), None);
    }

    #[test]
    fn nearest_name() {
        assert_eq!(hex_to_css_color_name("#ff0000"), Some("red"));
        assert_eq!(hex_to_css_color_name("#fe0101"), Some("red"));
        assert_eq!(hex_to_css_color_name("#4285f4"), Some("dodgerblue"));
        assert_eq!(hex_to_css_color_name("garbage"), None);
    }
}
