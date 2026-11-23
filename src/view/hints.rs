//! Picking an event by keyboard, as link hints do in Vimium and Tridactyl: `P` labels every
//! event in view with a few letters; typing them narrows the labels down until one is left,
//! and that event is selected. Backspace takes a letter back, Esc (or a click) gives up, and a
//! letter no label goes on with is ignored.

use gpui::{Context, FontWeight, div, prelude::*, px};

use super::CalmView;
use super::arrange::Pt;
use crate::keys::Press;
use crate::theme::with_alpha;

/// Home-row first, as Vimium's defaults.
const LETTERS: &str = "sadfjklewcmpgh";

pub(super) struct Hints {
    /// Event index and its label, in reading order.
    targets: Vec<(usize, String)>,
    typed: String,
}

/// `n` labels from `letters`, as short as they can be and none the start of another (so a
/// label is chosen the moment it's typed in full). Vimium's scheme.
pub fn labels(n: usize, letters: &str) -> Vec<String> {
    let letters: Vec<char> = letters.chars().collect();
    let mut hints = vec![String::new()];
    let mut offset = 0;
    while hints.len() - offset < n || hints.len() == 1 {
        let hint = hints[offset].clone();
        offset += 1;
        for &c in &letters {
            hints.push(format!("{c}{hint}"));
        }
    }
    let mut out: Vec<String> = hints[offset..offset + n].to_vec();
    out.sort();
    out.into_iter().map(|h| h.chars().rev().collect()).collect()
}

impl CalmView {
    pub(super) fn hinting(&self) -> bool {
        self.hints.is_some()
    }

    /// Labels every event in view (once per event, however many pieces it's drawn in).
    pub(super) fn start_hints(&mut self, cx: &mut Context<Self>) {
        let mut seen = std::collections::HashSet::new();
        let mut events: Vec<usize> = self
            .hits
            .iter()
            .filter(|(i, h)| !self.over_chrome(h.anchor()) && seen.insert(self.data.events[*i].group) && self.found.as_ref().is_none_or(|f| f.hits.get(*i).copied().unwrap_or(true)))
            .map(|(i, _)| *i)
            .collect();
        if events.is_empty() {
            self.toast("No events in view to pick".into(), None, cx);
            return;
        }
        // Reading order: by day, then time.
        events.sort_by(|&a, &b| {
            let (a, b) = (&self.data.events[a], &self.data.events[b]);
            (a.day, a.all_day.is_none(), a.start_min).partial_cmp(&(b.day, b.all_day.is_none(), b.start_min)).unwrap_or(std::cmp::Ordering::Equal)
        });
        let labels = labels(events.len(), LETTERS);
        self.hints = Some(Hints { targets: events.into_iter().zip(labels).collect(), typed: String::new() });
    }

    /// A key while hinting: all keys go here until it ends.
    pub(super) fn hint_key(&mut self, p: &Press) {
        let Some(h) = &mut self.hints else { return };
        match p.key.as_str() {
            "escape" => self.hints = None,
            "backspace" => {
                h.typed.pop();
            }
            _ => {
                let Some(c) = p.typed.as_deref().filter(|t| t.chars().count() == 1).map(str::to_lowercase) else { return };
                let typed = format!("{}{c}", h.typed);
                let left: Vec<usize> = h.targets.iter().filter(|(_, l)| l.starts_with(&typed)).map(|(i, _)| *i).collect();
                match left.as_slice() {
                    // Nothing goes on with that letter: ignore it.
                    [] => {}
                    [i] if h.targets.iter().any(|(_, l)| *l == typed) => {
                        self.selected = Some(*i);
                        self.hints = None;
                    }
                    _ => h.typed = typed,
                }
            }
        }
    }

    /// The labels, where their events are drawn now; typed letters dimmed, the ones ruled out
    /// gone.
    pub(super) fn hints_layer(&self) -> Option<impl IntoElement + use<>> {
        let h = self.hints.as_ref()?;
        let th = self.theme;
        let badges = h.targets.iter().filter(|(_, l)| l.starts_with(&h.typed)).filter_map(|(i, label)| {
            let (x, y) = self.hits.iter().find(|(j, _)| j == i)?.1.anchor();
            let rest = label[h.typed.len()..].to_uppercase();
            Some(
                div()
                    .absolute()
                    .left(px(x + 3.))
                    .top(px(y + 3.))
                    .px(px(4.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .bg(th.ink)
                    .shadow_md()
                    .flex()
                    .text_size(px(12.))
                    .font_weight(FontWeight::BOLD)
                    .child(div().text_color(with_alpha(th.bg, 0.45)).child(h.typed.to_uppercase()))
                    .child(div().text_color(th.bg).child(rest)),
            )
        });
        Some(div().absolute().top_0().left_0().right_0().bottom_0().children(badges))
    }
}

impl super::Hit {
    /// Where a label goes: the top left of the event.
    pub(super) fn anchor(&self) -> Pt {
        match self {
            super::Hit::Rect(b) => (b[0], b[1]),
            super::Hit::Poly(pts) => {
                let n = pts.len().max(1) as f32;
                let (x, y) = pts.iter().fold((0., 0.), |(x, y), p| (x + p.0, y + p.1));
                (x / n - 8., y / n - 8.)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_short_and_prefix_free() {
        assert_eq!(labels(3, "ab"), ["aa", "b", "ab"].map(String::from));
        for n in [1, 5, 14, 15, 60, 300] {
            let l = labels(n, LETTERS);
            assert_eq!(l.len(), n);
            for a in &l {
                assert!(!l.iter().any(|b| b != a && b.starts_with(a.as_str())), "{a} starts another, n={n}");
            }
            assert!(l.iter().all(|a| a.len() <= if n <= 14 { 1 } else if n <= 196 { 2 } else { 3 }), "n={n}: {l:?}");
        }
    }
}
