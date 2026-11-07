//! Understanding the date part of a search: "snow last winter", "beach june
//! 2024", "christmas". Those words become filters (and chips under the search
//! bar); whatever is left is described to the model.

const DAY: i64 = 86_400;

/// Days since 1970-01-01 for a civil date (proleptic Gregorian).
pub fn days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// (year, month 1-12, day) for days since 1970-01-01.
pub fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

const MONTHS: [&str; 12] = ["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];
const MONTH_NAMES: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

#[derive(Clone, Debug, PartialEq)]
pub enum Filter {
    /// [from, to) in unix seconds.
    Range(i64, i64),
    Month(i64),
    /// A season in any year: 0 spring, 1 summer, 2 autumn, 3 winter.
    Season(u8),
    /// Christmas week (20-31 Dec) in any year.
    Christmas,
    /// 30 Dec - 2 Jan in any year.
    NewYear,
}

impl Filter {
    pub fn accepts(&self, t: i64) -> bool {
        let (_, m, d) = civil(t.div_euclid(DAY));
        match *self {
            Filter::Range(a, b) => t >= a && t < b,
            Filter::Month(x) => m == x,
            Filter::Season(s) => season_of(m) == s,
            Filter::Christmas => m == 12 && d >= 20,
            Filter::NewYear => (m == 12 && d >= 30) || (m == 1 && d <= 2),
        }
    }
}

fn season_of(m: i64) -> u8 {
    match m {
        3..=5 => 0,
        6..=8 => 1,
        9..=11 => 2,
        _ => 3,
    }
}

const SEASONS: [&str; 4] = ["spring", "summer", "autumn", "winter"];

/// The span of season `s` that ends in `year` (winter runs Dec of year-1 to Feb).
fn season_range(s: u8, year: i64) -> (i64, i64) {
    let (a, b) = match s {
        0 => ((year, 3), (year, 6)),
        1 => ((year, 6), (year, 9)),
        2 => ((year, 9), (year, 12)),
        _ => ((year - 1, 12), (year, 3)),
    };
    (days(a.0, a.1, 1) * DAY, days(b.0, b.1, 1) * DAY)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Understood {
    /// What's left to describe to the model.
    pub text: String,
    pub filters: Vec<Filter>,
    /// One label per filter, for the chips.
    pub chips: Vec<String>,
}

impl Understood {
    pub fn accepts(&self, t: i64) -> bool {
        self.filters.iter().all(|f| f.accepts(t))
    }
}

/// `now` is unix seconds; it decides what "last summer" means.
pub fn understand(q: &str, now: i64) -> Understood {
    let (ty, tm, _) = civil(now.div_euclid(DAY));
    let today = now.div_euclid(DAY) * DAY;
    let words: Vec<String> = q.split_whitespace().map(|w| w.to_lowercase().trim_matches(|c: char| !c.is_alphanumeric() && c != '\'').to_string()).collect();
    let raw: Vec<&str> = q.split_whitespace().collect();
    let mut used = vec![false; words.len()];
    let mut found: Vec<(usize, Filter, String)> = vec![];
    let w = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    let mut i = 0;
    while i < words.len() {
        let (a, b) = (w(i), w(i + 1));
        let mut take = |n: usize, f: Filter, label: String, used: &mut Vec<bool>| {
            for k in i..i + n {
                used[k] = true;
            }
            found.push((i, f, label));
            n
        };
        let step = if (a == "last" || a == "this") && (SEASONS.contains(&b) || b == "fall") {
            let s = SEASONS.iter().position(|x| *x == b).unwrap_or(2) as u8;
            let mut y = ty + if s == 3 && tm == 12 { 1 } else { 0 };
            if a == "last" {
                // the most recent one that has finished
                while season_range(s, y).1 > now {
                    y -= 1;
                }
            }
            let (from, to) = season_range(s, y);
            let label = if s == 3 { format!("Winter {}–{:02}", y - 1, y % 100) } else { format!("{} {y}", cap(SEASONS[s as usize])) };
            take(2, Filter::Range(from, to), label, &mut used)
        } else if a == "last" && b == "year" {
            take(2, Filter::Range(days(ty - 1, 1, 1) * DAY, days(ty, 1, 1) * DAY), format!("{}", ty - 1), &mut used)
        } else if a == "this" && b == "year" {
            take(2, Filter::Range(days(ty, 1, 1) * DAY, days(ty + 1, 1, 1) * DAY), format!("{ty}"), &mut used)
        } else if a == "last" && b == "month" {
            let (y, m) = if tm == 1 { (ty - 1, 12) } else { (ty, tm - 1) };
            take(2, Filter::Range(days(y, m, 1) * DAY, days(ty, tm, 1) * DAY), format!("{} {y}", MONTH_NAMES[m as usize - 1]), &mut used)
        } else if a == "this" && b == "month" {
            take(2, Filter::Range(days(ty, tm, 1) * DAY, now + DAY), format!("{} {ty}", MONTH_NAMES[tm as usize - 1]), &mut used)
        } else if a == "last" && b == "week" {
            take(2, Filter::Range(today - 7 * DAY, now + DAY), "Last 7 days".into(), &mut used)
        } else if a == "today" {
            take(1, Filter::Range(today, today + DAY), "Today".into(), &mut used)
        } else if a == "yesterday" {
            take(1, Filter::Range(today - DAY, today), "Yesterday".into(), &mut used)
        } else if a == "new" && (b == "year" || b == "years" || b == "year's") {
            take(2, Filter::NewYear, "New Year's".into(), &mut used)
        } else if a == "christmas" || a == "xmas" {
            take(1, Filter::Christmas, "Christmas".into(), &mut used)
        } else if let Some(s) = SEASONS.iter().position(|x| *x == a).or((a == "fall").then_some(2)) {
            take(1, Filter::Season(s as u8), format!("Any {}", SEASONS[s]), &mut used)
        } else if let Some(m) = MONTHS
            .iter()
            .position(|x| *x == a || (a.len() == 3 && a != "may" && x.starts_with(a)))
            // "may" is only a month next to a year or after "in"
            .filter(|_| a != "may" || b.parse::<i64>().is_ok() || (i > 0 && w(i - 1) == "in"))
        {
            take(1, Filter::Month(m as i64 + 1), MONTH_NAMES[m].into(), &mut used)
        } else if let Some(y) = a.parse::<i64>().ok().filter(|y| (1990..=ty + 1).contains(y) && a.len() == 4) {
            take(1, Filter::Range(days(y, 1, 1) * DAY, days(y + 1, 1, 1) * DAY), format!("{y}"), &mut used)
        } else {
            1
        };
        i += step;
    }
    // "dog june 2024" reads as one chip, June 2024
    let mut merged: Vec<(usize, Filter, String)> = vec![];
    for (at, f, l) in found {
        if let (Some((_, Filter::Month(_), ml)), Filter::Range(..)) = (merged.last_mut(), &f) {
            if l.len() == 4 && l.chars().all(|c| c.is_ascii_digit()) {
                *ml = format!("{ml} {l}");
                merged.push((at, f, String::new()));
                continue;
            }
        }
        merged.push((at, f, l));
    }
    // Keep the rest, minus a connecting word left dangling ("beach in").
    const LINK: [&str; 7] = ["in", "from", "during", "on", "at", "of", "around"];
    let mut rest: Vec<&str> = raw.iter().zip(&used).filter(|(_, u)| !**u).map(|(r, _)| *r).collect();
    while rest.last().is_some_and(|x| LINK.contains(&x.to_lowercase().as_str())) && !merged.is_empty() {
        rest.pop();
    }
    Understood { text: rest.join(" "), filters: merged.iter().map(|m| m.1.clone()).collect(), chips: merged.into_iter().map(|m| m.2).filter(|l| !l.is_empty()).collect() }
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    // Saturday 7 November 2026, noon UTC
    const NOW: i64 = 1_794_052_800;

    #[test]
    fn calendar_round_trips() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(days(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(civil(NOW / DAY), (2026, 11, 7));
    }

    #[test]
    fn last_winter_is_the_one_that_ended() {
        let u = understand("snow last winter", NOW);
        assert_eq!(u.text, "snow");
        assert_eq!(u.chips, vec!["Winter 2025–26"]);
        assert!(u.accepts(days(2026, 1, 15) * DAY));
        assert!(!u.accepts(days(2025, 1, 15) * DAY));
    }

    #[test]
    fn last_summer_this_year() {
        let u = understand("beach last summer", NOW);
        assert_eq!(u.chips, vec!["Summer 2026"]);
        assert!(u.accepts(days(2026, 8, 1) * DAY));
    }

    #[test]
    fn month_and_year_merge() {
        let u = understand("dog in june 2024", NOW);
        assert_eq!(u.text, "dog");
        assert_eq!(u.chips, vec!["June 2024"]);
        assert!(u.accepts(days(2024, 6, 3) * DAY));
        assert!(!u.accepts(days(2025, 6, 3) * DAY));
        assert!(!u.accepts(days(2024, 7, 3) * DAY));
    }

    #[test]
    fn plain_words_stay() {
        let u = understand("may the force", NOW);
        assert_eq!(u.text, "may the force");
        assert!(u.filters.is_empty());
        let u = understand("sunset with friends", NOW);
        assert!(u.filters.is_empty());
        assert_eq!(u.text, "sunset with friends");
    }

    #[test]
    fn holidays() {
        let u = understand("new york at christmas", NOW);
        assert_eq!(u.text, "new york");
        assert!(u.accepts(days(2024, 12, 22) * DAY));
        assert!(!u.accepts(days(2024, 12, 2) * DAY));
    }
}
