//! Turning the parser's raw stat lines into GGG stat-template form.
//!
//! The rolled value is replaced by `#` (`#% to Lightning Resistance`). A sign
//! immediately before the placeholder is dropped on both sides so they match;
//! only ranged (rolled) numbers become `#`, with an all-numbers fallback.

use std::sync::OnceLock;

use regex::Regex;

/// A candidate template + the rolled values extracted from the stat line.
#[derive(Debug, Clone, PartialEq)]
pub struct Normalized {
    pub template: String,
    pub values: Vec<f64>,
}

/// A number, optionally followed by its `(min-max)` roll range.
fn roll_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"([+-]?\d+(?:\.\d+)?)\([^)]*\)").expect("roll regex"))
}

/// Any bare number (with or without a trailing range).
fn number_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"([+-]?\d+(?:\.\d+)?)(?:\([^)]*\))?").expect("number regex"))
}

/// Strip a sign immediately before a `#` so both sides match.
pub fn canonical_ggg(template: &str) -> String {
    template.replace("+#", "#").replace("-#", "#")
}

/// Candidate normalizations for a parser stat line, most-precise first.
pub fn candidates(stat: &str) -> Vec<Normalized> {
    let stat = strip_value_annotation(stat);
    let mut out = Vec::new();

    let ranged = roll_only(stat);
    out.push(ranged);

    let all = all_numbers(stat);
    if !out.iter().any(|c| c.template == all.template) {
        out.push(all);
    }

    out
}

/// Strip a trailing `— Unscalable Value` annotation (any dash) POE2 adds.
fn strip_value_annotation(stat: &str) -> &str {
    for dash in ['—', '–', '-'] {
        if let Some(idx) = stat.rfind(dash) {
            if stat[idx + dash.len_utf8()..]
                .trim()
                .eq_ignore_ascii_case("unscalable value")
            {
                return stat[..idx].trim_end();
            }
        }
    }
    stat
}

fn roll_only(stat: &str) -> Normalized {
    let mut values = Vec::new();
    for caps in roll_re().captures_iter(stat) {
        if let Some(v) = caps.get(1).and_then(|m| m.as_str().parse::<f64>().ok()) {
            values.push(v);
        }
    }
    let template = canonical_ggg(&roll_re().replace_all(stat, "#"));
    Normalized { template, values }
}

fn all_numbers(stat: &str) -> Normalized {
    let mut values = Vec::new();
    for caps in number_re().captures_iter(stat) {
        if let Some(v) = caps.get(1).and_then(|m| m.as_str().parse::<f64>().ok()) {
            values.push(v);
        }
    }
    let template = canonical_ggg(&number_re().replace_all(stat, "#"));
    Normalized { template, values }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn primary(stat: &str) -> Normalized {
        candidates(stat).into_iter().next().unwrap()
    }

    #[test]
    fn resistance_drops_sign_and_range() {
        let n = primary("+30(20-30)% to Lightning Resistance");
        assert_eq!(n.template, "#% to Lightning Resistance");
        assert_eq!(n.values, [30.0]);
    }

    #[test]
    fn flat_value_drops_sign() {
        let n = primary("+221(203-233) to Evasion Rating");
        assert_eq!(n.template, "# to Evasion Rating");
        assert_eq!(n.values, [221.0]);
    }

    #[test]
    fn leading_text_and_percent_kept() {
        let n = primary("Minions deal 24(22-24)% increased Damage");
        assert_eq!(n.template, "Minions deal #% increased Damage");
        assert_eq!(n.values, [24.0]);
    }

    #[test]
    fn hybrid_two_rolls() {
        let n = primary("Adds 5(4-6) to 12(10-14) Physical Damage");
        assert_eq!(n.template, "Adds # to # Physical Damage");
        assert_eq!(n.values, [5.0, 12.0]);
    }

    #[test]
    fn constant_stays_literal_when_ranged_roll_present() {
        let n = primary("+5(4-6) to maximum Life per 8 Armour on Equipped Helmet");
        assert_eq!(
            n.template,
            "# to maximum Life per 8 Armour on Equipped Helmet"
        );
        assert_eq!(n.values, [5.0]);
    }

    #[test]
    fn all_numbers_fallback_for_unranged_roll() {
        let cands = candidates("+1 to Level of all Tornado Shot Skills");
        assert!(cands
            .iter()
            .any(|c| c.template == "# to Level of all Tornado Shot Skills"));
    }

    #[test]
    fn strips_unscalable_value_annotation() {
        let n = primary("Minions' Strikes have Melee Splash — Unscalable Value");
        assert_eq!(n.template, "Minions' Strikes have Melee Splash");
        assert!(n.values.is_empty());
        let r = primary("Gains 0.17(0.15-0.20) Charges per Second — Unscalable Value");
        assert_eq!(r.template, "Gains # Charges per Second");
        assert_eq!(r.values, [0.17]);
    }

    #[test]
    fn flag_mod_without_numbers_is_unchanged() {
        let n = primary("Cannot be Frozen");
        assert_eq!(n.template, "Cannot be Frozen");
        assert!(n.values.is_empty());
    }

    #[test]
    fn canonical_ggg_strips_placeholder_sign() {
        assert_eq!(canonical_ggg("+# to maximum Life"), "# to maximum Life");
        assert_eq!(
            canonical_ggg("#% to Fire Resistance"),
            "#% to Fire Resistance"
        );
    }
}
