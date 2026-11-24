//! VALORANT competitive tiers and their terminal colors.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RankFamily {
    Unranked,
    Iron,
    Bronze,
    Silver,
    Gold,
    Platinum,
    Diamond,
    Ascendant,
    Immortal,
    Radiant,
}

/// Map a `competitiveTier` value (VAL-MATCH-V1) to its family.
/// 0–2 unranked, 3–5 Iron, 6–8 Bronze, … 24–26 Immortal, 27 Radiant.
pub fn family(tier: i32) -> RankFamily {
    match tier {
        3..=5 => RankFamily::Iron,
        6..=8 => RankFamily::Bronze,
        9..=11 => RankFamily::Silver,
        12..=14 => RankFamily::Gold,
        15..=17 => RankFamily::Platinum,
        18..=20 => RankFamily::Diamond,
        21..=23 => RankFamily::Ascendant,
        24..=26 => RankFamily::Immortal,
        t if t >= 27 => RankFamily::Radiant,
        _ => RankFamily::Unranked,
    }
}

pub fn tier_name(tier: i32) -> String {
    let fam = family(tier);
    let base = match fam {
        RankFamily::Unranked => return "Unranked".into(),
        RankFamily::Radiant => return "Radiant".into(),
        RankFamily::Iron => "Iron",
        RankFamily::Bronze => "Bronze",
        RankFamily::Silver => "Silver",
        RankFamily::Gold => "Gold",
        RankFamily::Platinum => "Platinum",
        RankFamily::Diamond => "Diamond",
        RankFamily::Ascendant => "Ascendant",
        RankFamily::Immortal => "Immortal",
    };
    format!("{base} {}", (tier - 3) % 3 + 1)
}

/// RGB color for a rank family.
pub fn rgb(fam: RankFamily) -> (u8, u8, u8) {
    match fam {
        RankFamily::Unranked => (150, 150, 150),
        RankFamily::Iron => (128, 128, 128),     // gray
        RankFamily::Bronze => (166, 110, 60),    // brown
        RankFamily::Silver => (211, 211, 211),   // light gray
        RankFamily::Gold => (240, 200, 40),      // yellow
        RankFamily::Platinum => (40, 200, 210),  // cyan
        RankFamily::Diamond => (175, 100, 235),  // purple
        RankFamily::Ascendant => (40, 190, 100), // green
        RankFamily::Immortal => (220, 50, 70),   // red
        RankFamily::Radiant => (255, 223, 90),   // bright gold
    }
}

pub fn color(tier: i32) -> ratatui::style::Color {
    let (r, g, b) = rgb(family(tier));
    ratatui::style::Color::Rgb(r, g, b)
}

pub fn term_color(tier: i32) -> crossterm::style::Color {
    let (r, g, b) = rgb(family(tier));
    crossterm::style::Color::Rgb { r, g, b }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names() {
        assert_eq!(tier_name(3), "Iron 1");
        assert_eq!(tier_name(14), "Gold 3");
        assert_eq!(tier_name(21), "Ascendant 1");
        assert_eq!(tier_name(26), "Immortal 3");
        assert_eq!(tier_name(27), "Radiant");
        assert_eq!(tier_name(0), "Unranked");
        assert_eq!(family(18), RankFamily::Diamond);
    }
}
