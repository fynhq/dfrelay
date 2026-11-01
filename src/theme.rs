use std::sync::OnceLock;

use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    Dark,
    Light,
}

impl Palette {
    pub fn auto() -> Self {
        *AUTO.get_or_init(detect_auto)
    }

    pub fn muted(self) -> Color {
        // ANSI bright black is a mid gray: clearly dimmer than default light
        // text on dark terminals and clearly lighter than default dark text on
        // light terminals. The old ANSI gray was nearly the default foreground
        // on many dark themes.
        match self {
            Self::Dark | Self::Light => Color::DarkGray,
        }
    }
}

static AUTO: OnceLock<Palette> = OnceLock::new();

fn detect_auto() -> Palette {
    std::env::var("COLORFGBG")
        .ok()
        .as_deref()
        .and_then(background_from_colorfgbg)
        .unwrap_or(Palette::Dark)
}

fn background_from_colorfgbg(value: &str) -> Option<Palette> {
    let background = value.rsplit(';').next()?.trim().parse::<u16>().ok()?;
    // ANSI background 7 and bright colors (9-15) are light; 0-6 and 8 are dark.
    Some(if background > 6 && background != 8 {
        Palette::Light
    } else {
        Palette::Dark
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_detection_reads_the_background_color() {
        assert_eq!(background_from_colorfgbg("15;0"), Some(Palette::Dark));
        assert_eq!(background_from_colorfgbg("0;15"), Some(Palette::Light));
        assert_eq!(background_from_colorfgbg("0;8"), Some(Palette::Dark));
        assert_eq!(background_from_colorfgbg("0;7"), Some(Palette::Light));
        assert_eq!(background_from_colorfgbg("default;default"), None);
        assert_eq!(background_from_colorfgbg("0"), Some(Palette::Dark));
    }

    #[test]
    fn muted_gray_is_readable_on_both_backgrounds() {
        assert_eq!(Palette::Dark.muted(), Color::DarkGray);
        assert_eq!(Palette::Light.muted(), Color::DarkGray);
    }
}
