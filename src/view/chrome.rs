//! Window chrome built from gpui elements: top bar, lens switcher, tooltip,
//! detail drawer, help. Positions and opacities come from our springs.

use std::time::Duration;

use chrono::Datelike;
use gpui::{
    Animation, AnimationExt, AnyElement, Context, FontWeight, Hsla, SharedString, Stateful, Div, div, ease_out_quint,
    prelude::*, px,
};

use super::scene::{DAY_NAMES, fmt_range};
use super::{CalmView, Stats};
use crate::color::{self, Measure};
use crate::model::Status;
use crate::spec::{Bind, Detail};
use crate::theme::with_alpha;

pub const TOPBAR_H: f32 = 64.;
pub const DRAWER_W: f32 = 320.;
const SWITCH_W: f32 = 76.;

fn hash(s: &str) -> usize {
    s.bytes().fold(5381usize, |h, b| h.wrapping_mul(33) ^ b as usize)
}

impl CalmView {
    fn button(&self, id: impl Into<gpui::ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
        let th = self.theme;
        div()
            .id(id)
            .h(px(30.))
            .px(px(12.))
            .flex()
            .items_center()
            .rounded(px(8.))
            .text_size(px(13.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(th.muted)
            .cursor_pointer()
            .hover(move |s| s.bg(with_alpha(th.ink, 0.06)).text_color(th.ink))
            .child(label.into())
    }

    /// Title and subtitle for whatever span the lens is heading to.
    fn title(&self) -> (String, String) {
        let d = &self.data;
        let l = self.lens();
        let n = l.zoom.target.exp2();
        let start = l.start.target.round() as i32;
        let end = (l.start.target + n - 1.).round() as i32;
        let center = d.date((l.start.target + n * 0.5) as i32);
        let (a, b) = (d.date(start), d.date(end));
        let range = |a: chrono::NaiveDate, b: chrono::NaiveDate| {
            if a.month() == b.month() {
                format!("{}–{} {}", a.day(), b.day(), a.format("%b"))
            } else {
                format!("{} – {}", a.format("%-d %b"), b.format("%-d %b"))
            }
        };
        if n < 1.5 {
            let rel = match start - d.today {
                0 => "Today · ".to_string(),
                1 => "Tomorrow · ".to_string(),
                -1 => "Yesterday · ".to_string(),
                _ => String::new(),
            };
            (a.format("%A").to_string(), format!("{rel}{}", a.format("%-d %B %Y")))
        } else if n < 10. {
            let mid = d.date(start + 3);
            (mid.format("%B %Y").to_string(), format!("Week {} · {}", mid.iso_week().week(), range(a, b)))
        } else if n < 50. {
            (center.format("%B %Y").to_string(), format!("{} weeks · {}", (n / 7.).round(), range(a, b)))
        } else if n < 300. {
            // Spans start on a Monday; name them by the days they mostly cover.
            let (a, b) = (d.date(start + 3), d.date(end - 3));
            let title = if a.year() == b.year() {
                format!("{} – {}", a.format("%b"), b.format("%b %Y"))
            } else {
                format!("{} – {}", a.format("%b %Y"), b.format("%b %Y"))
            };
            (title, format!("{} weeks", (n / 7.).round()))
        } else {
            (center.format("%Y").to_string(), format!("{} weeks · {} – {}", (n / 7.).round(), a.format("%-d %b"), b.format("%-d %b %Y")))
        }
    }

    pub(super) fn top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let th = self.theme;
        let (title, subtitle) = self.title();
        let sync_line = self.status_line();
        let key = hash(&title) ^ hash(&subtitle);
        let ChipLayout { row: primary, names, compact, .. } = self.chip_layout();
        let chips = primary.iter().enumerate().map(|(k, &c)| {
            let hidden = self.hidden[c];
            let dot = th.cals[c].dot;
            div()
                .id(("cal", c))
                .h(px(28.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(7.))
                .rounded_full()
                .border_1()
                .border_color(if hidden { th.border } else { gpui::transparent_black() })
                .bg(if hidden { gpui::transparent_black() } else { with_alpha(th.ink, 0.045) })
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if hidden { th.faint } else { th.muted })
                .cursor_pointer()
                .hover(move |s| s.bg(with_alpha(th.ink, 0.08)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_calendar(c);
                    cx.notify();
                }))
                .child(
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .when(!hidden, |d| d.bg(dot))
                        .when(hidden, |d| d.border_1().border_color(th.faint)),
                )
                .when(!compact, |d| d.child(names[k].clone()))
        });
        let can_pan = true;

        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(px(TOPBAR_H))
            .px(px(28.))
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).line_height(px(26.)).child(title))
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .text_size(px(12.5))
                            .text_color(th.faint)
                            .child(subtitle)
                            .children(sync_line.map(|(text, problem)| {
                                div().text_color(if problem { th.now } else { th.faint }).child(format!("· {text}"))
                            })),
                    )
                    .with_animation(
                        ("title", key),
                        Animation::new(Duration::from_millis(380)).with_easing(ease_out_quint()),
                        |el, t| el.opacity(t).mt(px(8. * (1. - t))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w_0()
                    .child(div().flex_none().child(self.clock(self.viewport.0 < 640.)))
                    .child(div().w(px(10.)).flex_none())
                    // If even the dots don't fit, the chips clip rather than push the buttons out.
                    .child(div().flex().items_center().gap(px(6.)).min_w_0().flex_shrink().overflow_hidden().children(chips))
                    .child(div().w(px(14.)).flex_none())
                    .when(can_pan, |d| {
                        d.child(self.button("prev", "‹").flex_none().text_size(px(18.)).on_click(cx.listener(|this, _, _, cx| {
                            this.pan(-1);
                            cx.notify();
                        })))
                        .child(self.button("today", "Today").flex_none().on_click(cx.listener(|this, _, _, cx| {
                            this.go_today();
                            cx.notify();
                        })))
                        .child(self.button("next", "›").flex_none().text_size(px(18.)).on_click(cx.listener(|this, _, _, cx| {
                            this.pan(1);
                            cx.notify();
                        })))
                        .child(self.hamburger(cx))
                    }),
            )
    }

    fn chip(&self, id: impl Into<gpui::ElementId>, label: impl Into<SharedString>, active: bool) -> Stateful<Div> {
        let th = self.theme;
        div()
            .id(id)
            .h(px(26.))
            .px(px(10.))
            .flex()
            .items_center()
            .rounded_full()
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if active { th.ink } else { th.faint })
            .when(active, |d| d.bg(with_alpha(th.ink, if th.dark { 0.12 } else { 0.07 })))
            .cursor_pointer()
            .hover(move |s| s.text_color(th.ink))
            .child(label.into())
    }

    fn bar(&self) -> Div {
        let th = self.theme;
        div()
            .flex()
            .items_center()
            .gap(px(2.))
            .p(px(3.))
            .rounded_full()
            .bg(th.surface)
            .border_1()
            .border_color(th.border)
            .shadow_sm()
    }

    /// Legend for a continuous colour scale; calendar colours are legended by the top-bar chips.
    fn legend(&self) -> Option<Div> {
        let th = self.theme;
        let enc = self.color();
        let aggregate = self.aggregate();
        // Events in calendar hues: one pale-to-full ramp per calendar.
        let hued = !aggregate && !enc.by_calendar && enc.palette == color::Palette::Calendar && enc.measure.per_event();
        if !hued && (enc.faceted() || !(aggregate || enc.measure.per_event())) {
            return None;
        }
        let (lo, hi) = enc.measure.legend();
        let lo = if hued && enc.measure.diverging_per_event() { "easy · neutral" } else { lo };
        let swatch = if hued {
            div().flex().flex_col().w(px(56.)).h(px(14.)).rounded(px(3.)).overflow_hidden().children((0..self.calendars.len()).filter(|&c| !self.hidden[c]).map(|c| {
                let dot = th.cals[c].dot;
                div().flex_1().w_full().bg(gpui::linear_gradient(90., gpui::linear_color_stop(color::mix(th.cell, dot, 0.1), 0.), gpui::linear_color_stop(dot, 1.)))
            }))
        } else if enc.measure.diverging_per_event() && !aggregate {
            div()
                .flex()
                .w(px(56.))
                .h(px(8.))
                .rounded_full()
                .overflow_hidden()
                .child(div().flex_1().h_full().bg(gpui::linear_gradient(90., gpui::linear_color_stop(color::diverging(&th, -color::EASY), 0.), gpui::linear_color_stop(th.cell, 1.))))
                .child(div().flex_1().h_full().bg(gpui::linear_gradient(90., gpui::linear_color_stop(th.cell, 0.), gpui::linear_color_stop(color::diverging(&th, 1.), 1.))))
        } else {
            let m = enc.measure;
            div().w(px(56.)).h(px(8.)).rounded_full().bg(gpui::linear_gradient(
                90.,
                gpui::linear_color_stop(color::ramp(&th, m, 0.), 0.),
                gpui::linear_color_stop(color::ramp(&th, m, 1.), 1.),
            ))
        };
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(8.))
                .text_size(px(10.5))
                .text_color(th.faint)
                .child(lo)
                .child(swatch.border_1().border_color(th.border))
                .child(hi),
        )
    }

    /// Controls generated from the view's declared params, per row in row order:
    /// segmented params as chip groups, toggles as single chips.
    fn param_parts(&self, cx: &mut Context<Self>) -> Vec<Vec<AnyElement>> {
        let th = self.theme;
        let (spec, values, mark) = (self.spec(), &self.values[self.preset], self.lens().mark);
        let caption = |s: String| div().px(px(8.)).text_size(px(11.)).text_color(th.faint).child(s);
        let divider = || div().w(px(1.)).h(px(16.)).mx(px(4.)).bg(th.border);
        let mut rows: std::collections::BTreeMap<u8, Vec<AnyElement>> = std::collections::BTreeMap::new();
        for p in spec.params.iter().filter(|p| spec.active(p, values, mark)) {
            let row = rows.entry(p.row).or_default();
            if !row.is_empty() {
                row.push(divider().into_any_element());
            }
            let current = spec.value(&p.name, values, mark);
            match p.bind {
                Bind::Segmented => {
                    if let Some(l) = &p.label {
                        row.push(caption(l.clone()).into_any_element());
                    }
                    for (i, o) in spec.options(p, values, mark).enumerate() {
                        let (name, value) = (p.name.clone(), o.value.clone());
                        let id = gpui::ElementId::NamedInteger(p.name.clone().into(), i as u64);
                        row.push(
                            self.chip(id, o.label.clone(), o.value == current)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_param(&name, &value);
                                    cx.notify();
                                }))
                                .into_any_element(),
                        );
                    }
                }
                Bind::Toggle => {
                    let on = current == "true";
                    let name = p.name.clone();
                    let label = p.label.clone().unwrap_or_else(|| p.name.clone());
                    row.push(
                        self.chip(gpui::ElementId::Name(p.name.clone().into()), label, on)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_param(&name, if on { "false" } else { "true" });
                                cx.notify();
                            }))
                            .into_any_element(),
                    );
                }
            }
        }
        rows.into_values().collect()
    }

    /// About how wide the view's controls are laid out in one bar (for fitting the bottom row).
    fn params_width(&self) -> f32 {
        let (spec, values, mark) = (self.spec(), &self.values[self.preset], self.lens().mark);
        let text = |s: &str| s.chars().count() as f32 * 6.9;
        let mut w = 10.;
        let mut items = 0;
        for p in spec.params.iter().filter(|p| spec.active(p, values, mark)) {
            items += 1;
            w += match p.bind {
                Bind::Segmented => {
                    p.label.as_deref().map_or(0., |l| 16. + text(l) * 0.92) + spec.options(p, values, mark).map(|o| 22. + text(&o.label)).sum::<f32>()
                }
                Bind::Toggle => 22. + text(p.label.as_deref().unwrap_or(&p.name)),
            };
        }
        if items == 0 {
            return 0.;
        }
        w + (items - 1) as f32 * 9. + if self.legend().is_some() { 190. } else { 0. }
    }

    /// The view's controls in one bar, legend last.
    fn params_inline(&self, cx: &mut Context<Self>) -> Div {
        let th = self.theme;
        let mut bar = self.bar();
        for (i, part) in self.param_parts(cx).into_iter().enumerate() {
            if i > 0 {
                bar = bar.child(div().w(px(1.)).h(px(16.)).mx(px(4.)).bg(th.border));
            }
            bar = bar.children(part);
        }
        if let Some(legend) = self.legend() {
            bar = bar.child(div().w(px(1.)).h(px(16.)).mx(px(4.)).bg(th.border)).child(legend);
        }
        bar
    }

    /// The same controls stacked a bar per row (row 1 nearest the bottom), for the popup.
    fn params_stacked(&self, cx: &mut Context<Self>) -> Div {
        let th = self.theme;
        // In the popup, a row wraps rather than run past the window.
        let max = self.viewport.0 - 32.;
        let mut bars: Vec<Div> = self
            .param_parts(cx)
            .into_iter()
            .map(|part| self.bar().flex_wrap().justify_center().rounded(px(18.)).max_w(px(max)).children(part))
            .collect();
        if let (Some(legend), Some(top)) = (self.legend(), bars.last_mut()) {
            *top = std::mem::replace(top, div()).child(div().w(px(1.)).h(px(16.)).mx(px(4.)).bg(th.border)).child(legend);
        }
        div().flex().flex_col().items_center().gap(px(6.)).children(bars.into_iter().rev())
    }

    /// The bottom row: view picker at the left, the view's controls in the centre (or a
    /// button opening them, when they don't fit), the zoom strip at the right.
    pub(super) fn switcher(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let th = self.theme;
        let BottomLayout { item, params, params_x, .. } = self.bottom_layout();
        let items = self.views.iter().enumerate().map(|(i, p)| {
            let active = i == self.preset;
            div()
                .id(("lens", i))
                .w(px(item))
                .h(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .overflow_hidden()
                .text_size(px(if item < 60. { 11. } else { 12.5 }))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if active { th.ink } else { th.faint })
                .cursor_pointer()
                .hover(move |s| s.text_color(th.ink))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_preset(i);
                    cx.notify();
                }))
                // Very narrow: the first two letters.
                .child(if item < 44. { p.name.chars().take(2).collect::<String>() } else { p.name.clone() })
        });
        let picker = div()
            .absolute()
            .bottom(px(18.))
            .left(px(16.))
            .p(px(4.))
            .flex()
            .rounded_full()
            .bg(th.surface)
            .border_1()
            .border_color(th.border)
            .shadow_sm()
            .child(
                // Sliding selection, driven by a spring so it can be interrupted.
                div()
                    .absolute()
                    .top(px(4.))
                    .left(px(4. + self.switcher.x * item))
                    .w(px(item))
                    .h(px(30.))
                    .rounded_full()
                    .bg(with_alpha(th.ink, if th.dark { 0.12 } else { 0.07 })),
            )
            .children(items);
        let controls = match params {
            Params::None => None,
            Params::Inline => Some(div().absolute().bottom(px(21.)).left(px(params_x)).child(self.params_inline(cx))),
            Params::Button(tier) => Some(
                div().absolute().bottom(px(21.)).left(px(params_x)).child(
                    self.bar().child(
                        self.chip("params-button", self.params_label(tier), self.params_open)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.params_open = !this.params_open;
                                cx.notify();
                            })),
                    ),
                ),
            ),
        };
        // The popup: every control, stacked, above the row.
        let popup = (matches!(params, Params::Button(_)) && self.params_open).then(|| {
            div()
                .absolute()
                .bottom(px(18. + 40. + 8.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(div().on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(self.params_stacked(cx)))
        });
        div().absolute().top_0().left_0().right_0().bottom_0().child(picker).children(controls).children(popup)
    }

    /// The collapsed controls' button: "Colour: Calendar ▴", "Calendar ▴", or "⋯".
    fn params_label(&self, tier: u8) -> String {
        let (label, value) = self.params_summary();
        match tier {
            0 => format!("{label}: {value} ▴"),
            1 => format!("{value} ▴"),
            _ => "⋯".into(),
        }
    }

    /// The first choice among the view's controls, and what it's set to.
    fn params_summary(&self) -> (String, String) {
        let (spec, values, mark) = (self.spec(), &self.values[self.preset], self.lens().mark);
        spec.params
            .iter()
            .filter(|p| spec.active(p, values, mark) && p.bind == Bind::Segmented)
            .find_map(|p| {
                let v = spec.value(&p.name, values, mark);
                let o = spec.options(p, values, mark).find(|o| o.value == v)?;
                Some((p.label.clone().unwrap_or_else(|| p.name.clone()), o.label.clone()))
            })
            .unwrap_or_else(|| ("Controls".into(), "Controls".into()))
    }

    pub(super) fn tooltip_card(&self) -> Option<impl IntoElement + use<>> {
        let t = self.tooltip.x;
        if t < 0.01 {
            return None;
        }
        let th = self.theme;
        let (title, line1, line2, dot): (SharedString, String, Option<SharedString>, Hsla) = if let Some(zone) = self.hover_zone {
            let info = self.zones.info(zone);
            (info.name.into(), info.now, Some(info.detail.into()), th.faint)
        } else if let Some(i) = self.hovered {
            let e = &self.data.events[i];
            let date = self.data.date(e.day);
            let pieces = self.group_of(e.group);
            let when = match (pieces.first(), pieces.last()) {
                // Crosses midnight: from the first piece's start to the last piece's end.
                (Some(&a), Some(&b)) if a != b => {
                    let (a, b) = (&self.data.events[a], &self.data.events[b]);
                    let at = |d: i32, min: f32| {
                        let date = self.data.date(d);
                        let t = chrono::NaiveTime::from_num_seconds_from_midnight_opt((min.round() as u32 % 1440) * 60, 0).unwrap_or_default();
                        format!("{} {} {}", DAY_NAMES[d.rem_euclid(7) as usize], date.day(), t.format("%-I:%M %P"))
                    };
                    format!("{} → {}", at(a.day, a.start_min), at(b.day, b.end_min()))
                }
                _ => format!("{} {} · {}", DAY_NAMES[e.day.rem_euclid(7) as usize], date.day(), fmt_range(e.start_min, e.end_min())),
            };
            let strained = self.preset == 2 && crate::model::strain(e) >= crate::model::STRAIN;
            let line2 = if strained { Some("Above your usual energy at this hour".into()) } else { e.location.clone() };
            (e.title.clone(), when, line2, th.cals[e.calendar].dot)
        } else {
            let day = self.hover_day?;
            let s = self.cells.get(day as usize)?;
            let date = self.data.date(day);
            let title = date.format("%A %-d %B").to_string().into();
            let visible: Vec<usize> = (0..self.calendars.len()).filter(|&c| !self.hidden[c]).collect();
            let hours: f32 = visible.iter().map(|&c| s.hours[c]).sum();
            let count: u32 = visible.iter().map(|&c| s.count[c]).sum();
            let summary = format!("{count} events · {hours:.1}h booked · {:.1}h free 9–5", s.free.max(0.));
            // Per-calendar breakdown of whatever the colour encodes.
            let m = self.color().measure;
            let mut parts: Vec<(usize, f32)> = visible.iter().map(|&c| (c, s.part(m, c))).filter(|p| p.1.abs() > 0.01).collect();
            parts.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
            let fmt = |v: f32| match m {
                Measure::Count => format!("{}", v as u32),
                Measure::Demand => format!("{v:.1} load"),
                Measure::Strain => format!("{v:.1}h strained"),
                _ => format!("{v:.1}h"),
            };
            let breakdown = parts.iter().map(|&(c, v)| format!("{} {}", self.calendars[c].display(), fmt(v))).collect::<Vec<_>>().join(" · ");
            let dot = parts.first().map_or(th.cell, |&(c, _)| th.cals[c].dot);
            let span = s.bounds(&self.hidden).map(|(a, b)| format!("{} – {} · {:.1}h span", super::scene::fmt_time(a), super::scene::fmt_time(b), (b - a) / 60.));
            let timing = matches!(m, Measure::Starts | Measure::Ends | Measure::Span) || self.detail() == Detail::DaySpan;
            let line2 = match (timing, span) {
                (true, Some(span)) => span,
                _ if breakdown.is_empty() || !m.facetable() => "Click to open the week".into(),
                _ => breakdown,
            };
            (title, summary, Some(line2.into()), dot)
        };
        let (w, h) = (280., 90.);
        let mut x = self.mouse.0 + 16.;
        let mut y = self.mouse.1 + 18.;
        if x + w > self.viewport.0 - 8. {
            x = self.mouse.0 - w - 12.;
        }
        if y + h > self.viewport.1 - 8. {
            y = self.mouse.1 - h - 12.;
        }
        Some(
            div()
                .absolute()
                .left(px(x))
                .top(px(y + 4. * (1. - t)))
                .w(px(w))
                .opacity(t.clamp(0., 1.))
                .px(px(12.))
                .py(px(10.))
                .flex()
                .flex_col()
                .gap(px(3.))
                .rounded(px(10.))
                .bg(th.surface)
                .border_1()
                .border_color(th.border)
                .shadow_md()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(div().size(px(8.)).rounded_full().bg(dot))
                        .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).truncate().child(title)),
                )
                .child(div().text_size(px(12.)).text_color(th.muted).pl(px(16.)).child(line1))
                .children(line2.map(|l| div().text_size(px(12.)).text_color(th.faint).pl(px(16.)).child(l))),
        )
    }

    pub(super) fn drawer_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let t = self.drawer.x;
        if t < 0.005 {
            return None;
        }
        let th = self.theme;
        let i = self.shown?;
        let e = &self.data.events[i];
        let c = th.cals[e.calendar];
        let date = self.data.date(e.day);
        // The event's time in each extra zone, flagging a different day.
        let zone_rows: Vec<_> = if self.zones_on { self.zones.extra.clone() } else { Vec::new() }
            .into_iter()
            .map(|tz| {
                let start = e.start_min + self.zones.delta(tz, date) as f32;
                let day = match start.div_euclid(1440.) as i32 {
                    0 => "",
                    d if d > 0 => " · next day",
                    _ => " · previous day",
                };
                let local = start.rem_euclid(1440.);
                let text = format!("{} {}{day}", self.zones.label(tz, date), fmt_range(local, local + e.dur_min));
                div().text_size(px(12.)).text_color(th.faint).child(text)
            })
            .collect();
        let overlaps: Vec<SharedString> = self
            .data
            .events
            .iter()
            .enumerate()
            .filter(|(j, o)| *j != i && !self.hidden[o.calendar] && o.overlaps(e))
            .map(|(_, o)| format!("{} · {}", o.title, fmt_range(o.start_min, o.end_min())).into())
            .collect();
        let section = |label: &'static str| {
            div().text_size(px(11.)).font_weight(FontWeight::MEDIUM).text_color(th.faint).child(label.to_uppercase())
        };
        let row = |text: SharedString| div().text_size(px(13.)).text_color(th.muted).child(text);

        Some(
            div()
                .id("drawer")
                .absolute()
                .top(px(TOPBAR_H + 8.))
                .bottom(px(100.))
                .right(px(20. - (1. - t) * (DRAWER_W + 40.)))
                .w(px(DRAWER_W))
                .opacity(t.clamp(0., 1.))
                .p(px(22.))
                .flex()
                .flex_col()
                .gap(px(18.))
                .rounded(px(16.))
                .bg(th.surface)
                .border_1()
                .border_color(th.border)
                .shadow_lg()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .items_start()
                        .gap(px(12.))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(8.))
                                        .child(div().size(px(9.)).rounded_full().bg(c.dot))
                                        .child(div().text_size(px(12.)).text_color(th.muted).child(self.calendars.get(e.calendar).map_or(String::new(), |c| c.display().to_string())))
                                        .when(e.status == Status::Tentative, |d| {
                                            d.child(
                                                div()
                                                    .px(px(7.))
                                                    .py(px(1.))
                                                    .rounded_full()
                                                    .border_1()
                                                    .border_color(with_alpha(c.edge, 0.6))
                                                    .text_size(px(11.))
                                                    .text_color(th.muted)
                                                    .child("Tentative"),
                                            )
                                        }),
                                )
                                .child(div().text_size(px(21.)).font_weight(FontWeight::SEMIBOLD).line_height(px(27.)).child(e.title.clone())),
                        )
                        .child(self.button("close", "✕").text_size(px(13.)).on_click(cx.listener(|this, _, _, cx| {
                            this.selected = None;
                            cx.notify();
                        }))),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(div().text_size(px(15.)).font_weight(FontWeight::MEDIUM).child(date.format("%A %-d %B").to_string()))
                        .child(row(format!("{} · {} min", fmt_range(e.start_min, e.end_min()), e.dur_min as i32).into()))
                        .children(zone_rows)
                        .children(e.location.clone().map(row)),
                )
                .child({
                    let yours = self.data.energy_at(e.day, (e.start_min + e.end_min()) * 0.5);
                    let meter = |label: &'static str, level: f32, color: Hsla| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(div().w(px(84.)).text_size(px(12.)).text_color(th.muted).child(label))
                            .child(
                                div()
                                    .flex_1()
                                    .h(px(6.))
                                    .rounded_full()
                                    .bg(with_alpha(th.ink, 0.07))
                                    .child(div().h_full().w(gpui::relative(level.clamp(0.04, 1.))).rounded_full().bg(color)),
                            )
                    };
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(section("Energy"))
                        .child(meter("Demand", e.energy, c.edge))
                        .child(meter("Yours then", yours, th.energy))
                        .when(crate::model::strain(e) >= crate::model::STRAIN, |d| {
                            d.child(div().text_size(px(12.)).text_color(th.now).child("Demanding work in a usual energy dip"))
                        })
                })
                .children(e.notes.clone().map(|n| div().flex().flex_col().gap(px(6.)).child(section("Notes")).child(row(n))))
                .when(!overlaps.is_empty(), |d| {
                    d.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(section("Overlaps with"))
                            .children(overlaps.into_iter().map(|o| row(o).text_color(th.now))),
                    )
                }),
        )
    }

    pub(super) fn help_card(&self) -> Option<impl IntoElement + use<>> {
        let t = self.help.x;
        if t < 0.01 {
            return None;
        }
        let th = self.theme;
        let row = |k: String, v: String, dim: bool| {
            div()
                .flex()
                .justify_between()
                .text_size(px(13.))
                .when(dim, |d| d.opacity(0.45))
                .child(div().text_color(th.muted).child(v))
                .child(div().font_weight(FontWeight::MEDIUM).child(k))
        };
        let general = [
            ("1 – 9, Tab", "Switch view"),
            ("Ctrl+scroll, − =", "Zoom (day … year)"),
            ("← →", "Previous / next"),
            ("Scroll, ↑ ↓", "Scroll hours or time"),
            ("Alt+scroll", "Zoom hours"),
            ("F, click hour labels", "Full day / default hours"),
            ("T", "Jump to today"),
            ("E", "Edit this view"),
            ("M", "Menu: more calendars, actions"),
            ("Z", "Extra time zones"),
            ("R", "Sync calendars now"),
            ("D", "Light / dark"),
            ("Click", "Details · open a day or week"),
            ("Esc", "Close"),
            ("`", "Frame stats"),
        ]
        .map(|(k, v)| row(k.into(), v.into(), false));
        // This view's own keys, from its params; dimmed when not offered at the current zoom.
        let (spec, values, mark) = (self.spec(), &self.values[self.preset], self.lens().mark);
        let view_keys: Vec<_> = spec
            .params
            .iter()
            .filter_map(|p| {
                let k = p.key.as_ref()?;
                let what = p.label.clone().unwrap_or_else(|| p.name.clone());
                let what = if p.bind == Bind::Toggle { format!("{what} on / off") } else { format!("Cycle {}", what.to_lowercase()) };
                Some(row(k.to_uppercase(), what, !spec.active(p, values, mark)))
            })
            .collect();
        let view_title = (!view_keys.is_empty()).then(|| {
            div().mt(px(8.)).text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(th.faint).child(format!("{} view", spec.name).to_uppercase())
        });
        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(with_alpha(th.shadow, t))
                .child(
                    div()
                        .w(px(380.))
                        .opacity(t.clamp(0., 1.))
                        .mt(px(12. * (1. - t)))
                        .p(px(24.))
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .rounded(px(16.))
                        .bg(th.surface)
                        .border_1()
                        .border_color(th.border)
                        .shadow_lg()
                        .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).mb(px(4.)).child("Keyboard"))
                        .children(general)
                        .children(view_title)
                        .children(view_keys),
                ),
        )
    }

    pub(super) fn hud(&self) -> Option<impl IntoElement + use<>> {
        if !self.hud_on {
            return None;
        }
        let iv = Stats::recent(&self.stats.intervals);
        let cpu = Stats::recent(&self.stats.cpu);
        let line = if self.animating {
            format!(
                "{} · {} events · {} quads {} strips {} labels · frame p50 {:.1} p99 {:.1} ms · cpu p50 {:.2} p99 {:.2} ms",
                self.spec().describe(self.lens().mark),
                self.data.events.len(),
                self.drawn.0,
                self.drawn.1,
                self.drawn.2,
                Stats::pct(iv, 0.5),
                Stats::pct(iv, 0.99),
                Stats::pct(cpu, 0.5),
                Stats::pct(cpu, 0.99),
            )
        } else {
            format!("{} · {} events · idle", self.spec().describe(self.lens().mark), self.data.events.len())
        };
        Some(div().absolute().bottom(px(6.)).right(px(12.)).text_size(px(10.5)).text_color(self.theme.faint).child(line))
    }
}


impl CalmView {
    /// Zoom stops on the right edge; the highlight follows the continuous zoom.
    /// Zoom stops as a horizontal strip, bottom right beside the view switcher. Where full
    /// names would crowd the switcher, they shorten to initials.
    pub(super) fn zoom_slider(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let th = self.theme;
        let spec = self.spec();
        let logs = super::stops_log(spec);
        let z = self.lens().zoom.x;
        let pos = if z <= logs[0] {
            0.
        } else {
            let k = logs.windows(2).position(|w| z <= w[1]).unwrap_or(logs.len().saturating_sub(2));
            let (a, b) = (logs[k], logs.get(k + 1).copied().unwrap_or(logs[k] + 1.));
            (k as f32 + ((z - a) / (b - a)).clamp(0., 1.)).min((logs.len() - 1) as f32)
        };
        let nearest = pos.round() as usize;
        let BottomLayout { short, zoom_x, .. } = self.bottom_layout();
        let item = if short { 26. } else { 58. };
        let items = spec.stops.iter().enumerate().map(|(i, s)| {
            let label: String = if short { s.name.chars().take(1).collect() } else { s.name.clone() };
            div()
                .id(("stop", i))
                .w(px(item))
                .h(px(26.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if i == nearest { th.ink } else { th.faint })
                .cursor_pointer()
                .hover(move |st| st.text_color(th.ink))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.zoom_to_stop(i, None);
                    cx.notify();
                }))
                .child(label)
        });
        div()
            .absolute()
            .left(px(zoom_x))
            // Level with the view picker (40 px tall, 18 px up).
            .bottom(px(21.))
            .p(px(3.))
            .flex()
            .rounded_full()
            .bg(th.surface)
            .border_1()
            .border_color(th.border)
            .shadow_sm()
            .child(
                div()
                    .absolute()
                    .left(px(3. + pos * item))
                    .top(px(3.))
                    .w(px(item))
                    .h(px(26.))
                    .rounded_full()
                    .bg(with_alpha(th.ink, if th.dark { 0.12 } else { 0.07 })),
            )
            .children(items)
    }
}

/// One editable channel: caption, KDL child, optional property, (label, KDL value) options.
type EditRow = (&'static str, &'static str, Option<&'static str>, &'static [(&'static str, &'static str)]);

const EDIT_ROWS: [EditRow; 10] = [
    ("Days per line", "facet", Some("per-line"), &[("1", "1"), ("7", "7"), ("14", "14")]),
    ("Lines run", "facet", Some("dir"), &[("Down", "rows"), ("Across", "columns")]),
    ("Coordinates", "coord", None, &[("Cartesian", "cartesian"), ("Polar", "polar")]),
    ("Across (x)", "x", None, &[("Time", "time-of-day"), ("Lane", "lane"), ("Calendar", "calendar"), ("Demand", "demand"), ("None", "none")]),
    ("Down (y)", "y", None, &[("Time", "time-of-day"), ("Lane", "lane"), ("Calendar", "calendar"), ("Demand", "demand"), ("None", "none")]),
    ("Overlaps", "collide", None, &[("Side by side", "split"), ("Conflicts", "highlight-conflict"), ("Overlay", "overlay")]),
    ("Mark", "mark", None, &[("Block", "block"), ("Pill", "pill"), ("List", "list"), ("Heat", "heat")]),
    ("Time zones", "zones", None, &[("All", "all"), ("Primary", "primary")]),
    ("Rail", "rail", Some("show"), &[("Off", "off"), ("Energy", "energy"), ("Daylight", "daylight")]),
    ("Rail style", "rail", Some("style"), &[("Width + colour", "rail"), ("Colour", "strip")]),
];

impl CalmView {
    /// The KDL value a view currently has for an editor row.
    fn edit_current(spec: &crate::spec::Spec, child: &str, prop: Option<&str>) -> String {
        use crate::spec::{Axis, Collide, Coord, Dir, Mark, ZoneAxes};
        // A literal value as written; a `$param` shows as no choice selected.
        let lit = |r: &crate::spec::Ref| match r {
            crate::spec::Ref::Lit(v) => v.clone(),
            crate::spec::Ref::Param(_) => String::new(),
        };
        let axis = |a: Axis| match a {
            Axis::TimeOfDay => "time-of-day",
            Axis::Lane => "lane",
            Axis::Calendar => "calendar",
            Axis::Demand => "demand",
            Axis::None => "none",
        };
        match (child, prop) {
            ("facet", Some("per-line")) => spec.facet.per_line.to_string(),
            ("facet", _) => (if spec.facet.dir == Dir::Rows { "rows" } else { "columns" }).into(),
            ("coord", _) => (if spec.coord == Coord::Polar { "polar" } else { "cartesian" }).into(),
            ("x", _) => axis(spec.x).into(),
            ("y", _) => axis(spec.y).into(),
            ("collide", _) => match spec.collide {
                Collide::Split => "split",
                Collide::HighlightConflict => "highlight-conflict",
                Collide::Overlay => "overlay",
            }
            .into(),
            ("mark", _) => match spec.mark {
                Mark::Block => "block",
                Mark::Pill => "pill",
                Mark::List => "list",
                Mark::Heat => "heat",
            }
            .into(),
            ("zones", _) => (if spec.zones == ZoneAxes::Primary { "primary" } else { "all" }).into(),
            ("rail", Some("show")) => lit(&spec.rail),
            ("rail", _) => lit(&spec.rail_style),
            _ => String::new(),
        }
    }

    pub(super) fn editor_panel(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let t = self.editor.x;
        if t < 0.01 {
            return None;
        }
        let th = self.theme;
        let spec = self.spec();
        let name = spec.name.clone();
        let edited = self.conf.generated_names.contains(&name);
        let made_here = !self.conf.base_names.contains(&name);
        let rows = EDIT_ROWS.iter().map(|&(caption, child, prop, options)| {
            let current = Self::edit_current(spec, child, prop);
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().text_size(px(11.5)).text_color(th.faint).child(caption))
                .child(div().flex().flex_wrap().gap(px(2.)).children(options.iter().map(|&(label, value)| {
                    let id = gpui::ElementId::Name(format!("edit-{child}-{}-{value}", prop.unwrap_or("")).into());
                    self.chip(id, label, value == current).on_click(cx.listener(move |this, _, _, cx| {
                        this.edit_channel(child, prop, value);
                        cx.notify();
                    }))
                })))
        });
        let note = self.editor_note.clone().map(|(text, error)| {
            div().text_size(px(11.5)).text_color(if error { th.now } else { th.faint }).child(text)
        });
        Some(
            div()
                .id("editor")
                .absolute()
                .top(px(76.))
                .right(px(16.))
                .w(px(330.))
                // Scrolls when taller than the window.
                .max_h(px((self.viewport.1 - 76. - 16.).max(200.)))
                .overflow_y_scroll()
                .opacity(t.clamp(0., 1.))
                .ml(px(16. * (1. - t)))
                .p(px(18.))
                .flex()
                .flex_col()
                .gap(px(10.))
                .rounded(px(16.))
                .bg(th.surface)
                .border_1()
                .border_color(th.border)
                .shadow_lg()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .items_center()
                        .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(format!("Edit “{name}”")))
                        .child(self.button("editor-close", "✕").text_size(px(13.)).on_click(cx.listener(|this, _, _, cx| {
                            this.editor_on = false;
                            cx.notify();
                        }))),
                )
                .child(div().text_size(px(11.5)).text_color(th.muted).child(spec.describe(spec.mark)))
                .children(rows)
                .child(self.energy_settings(cx))
                .child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .mt(px(4.))
                        .child(self.button("editor-dup", "Duplicate").on_click(cx.listener(|this, _, _, cx| {
                            this.duplicate_current();
                            cx.notify();
                        })))
                        .when(edited, |d| {
                            d.child(self.button("editor-reset", if made_here { "Delete" } else { "Reset" }).on_click(cx.listener(|this, _, _, cx| {
                                this.reset_current();
                                cx.notify();
                            })))
                        }),
                )
                .child(div().text_size(px(11.)).text_color(th.faint).child("Saved to views.generated.kdl · your calm.kdl is never rewritten"))
                .children(note),
        )
    }
}

impl CalmView {
    /// The time here, and beneath it the time in each extra zone.
    fn clock(&self, compact: bool) -> Div {
        let th = self.theme;
        let now = chrono::Utc::now();
        let here = now.with_timezone(&chrono::Local);
        let others = (!compact).then(|| {
            self.zones.extra.iter().map(|tz| {
                let t = now.with_timezone(tz);
                let day = match (t.date_naive() - here.date_naive()).num_days() {
                    d if d > 0 => " +1",
                    d if d < 0 => " −1",
                    _ => "",
                };
                format!("{} {}{day}", self.zones.label(*tz, t.date_naive()), t.format("%-I:%M %P"))
            })
        });
        div()
            .flex()
            .flex_col()
            .items_end()
            .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(here.format("%-I:%M %P").to_string()))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .text_size(px(11.5))
                    .text_color(th.faint)
                    .children(others.into_iter().flatten()),
            )
    }
}

impl CalmView {
    /// The editor's energy section: settings for you, shared by every view.
    fn energy_settings(&self, cx: &mut Context<Self>) -> Div {
        use crate::energy::Baseline;
        let th = self.theme;
        let caption = |s: &'static str| div().text_size(px(11.5)).text_color(th.faint).child(s);
        let hm = |h: f32| {
            let m = (h * 60.).round() as i32;
            format!("{}:{:02}", m / 60 % 24, m % 60)
        };
        let reactive = self.energy.reactive;
        let mut section = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .mt(px(6.))
            .pt(px(10.))
            .border_t_1()
            .border_color(th.border)
            .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).child("Your energy · all views"))
            .child(caption("Reacts to your day"))
            .child(
                div()
                    .flex()
                    .gap(px(2.))
                    .child(self.chip("energy-react-on", "On", reactive).on_click(cx.listener(|this, _, _, cx| {
                        this.edit_energy("reactive", kdl::KdlValue::Bool(true));
                        cx.notify();
                    })))
                    .child(self.chip("energy-react-off", "Off", !reactive).on_click(cx.listener(|this, _, _, cx| {
                        this.edit_energy("reactive", kdl::KdlValue::Bool(false));
                        cx.notify();
                    }))),
            );
        match self.energy.baseline {
            Baseline::ThreeProcess { wake, sleep, .. } => {
                for (key, label, at) in [("wake", "Wake", wake), ("sleep", "Sleep", sleep)] {
                    // Half-hour steps, wrapping round midnight.
                    let shifted = move |delta: f32| {
                        let m = ((at + delta).rem_euclid(24.) * 60.).round() as i32;
                        format!("{}:{:02}", m / 60, m % 60)
                    };
                    let (down, up) = (shifted(-0.5), shifted(0.5));
                    section = section.child(caption(label)).child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .child(self.chip(gpui::ElementId::Name(format!("energy-{key}-down").into()), "−", false).on_click(cx.listener(move |this, _, _, cx| {
                                this.edit_energy(key, kdl::KdlValue::String(down.clone()));
                                cx.notify();
                            })))
                            .child(div().w(px(52.)).flex().justify_center().text_size(px(12.5)).font_weight(FontWeight::MEDIUM).child(hm(at)))
                            .child(self.chip(gpui::ElementId::Name(format!("energy-{key}-up").into()), "+", false).on_click(cx.listener(move |this, _, _, cx| {
                                this.edit_energy(key, kdl::KdlValue::String(up.clone()));
                                cx.notify();
                            }))),
                    );
                }
                section.child(div().text_size(px(11.)).text_color(th.faint).child("Model: three-process alertness (Åkerstedt & Folkard)"))
            }
            Baseline::Points(_) => section.child(div().text_size(px(11.)).text_color(th.faint).child("Your own curve, from calm.kdl")),
        }
    }
}

impl CalmView {
    /// Three bars, top right: opens the menu.
    fn hamburger(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let th = self.theme;
        let bar = || div().w(px(16.)).h(px(1.75)).rounded_full().bg(if self.menu_on { th.ink } else { th.muted });
        self.button("menu", "")
            .flex_none()
            .ml(px(4.))
            .w(px(34.))
            .justify_center()
            .when(self.menu_on, |d| d.bg(with_alpha(th.ink, 0.07)))
            .child(div().flex().flex_col().gap(px(3.5)).child(bar()).child(bar()).child(bar()))
            .on_click(cx.listener(|this, _, _, cx| {
                this.menu_on = !this.menu_on;
                cx.notify();
            }))
    }

    /// The top-right menu: secondary calendars, then everyday actions with their keys.
    pub(super) fn menu_panel(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let t = self.menu.x;
        if t < 0.01 {
            return None;
        }
        let th = self.theme;
        let heading = |s: &'static str| div().px(px(10.)).pt(px(6.)).pb(px(2.)).text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(th.faint).child(s.to_uppercase());
        let row = |id: gpui::ElementId| {
            div()
                .id(id)
                .h(px(32.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(px(8.))
                .text_size(px(13.))
                .text_color(th.ink)
                .cursor_pointer()
                .hover(move |s| s.bg(with_alpha(th.ink, 0.06)))
        };
        let secondary = self.chip_layout().menu;
        let calendars = secondary.iter().map(|&c| {
            let hidden = self.hidden[c];
            let dot = th.cals[c].dot;
            row(gpui::ElementId::Name(format!("menu-cal-{c}").into()))
                .child(div().size(px(8.)).rounded_full().when(!hidden, |d| d.bg(dot)).when(hidden, |d| d.border_1().border_color(th.faint)))
                .child(div().flex_1().text_color(if hidden { th.faint } else { th.ink }).child(self.calendars[c].display().to_string()))
                .child(div().text_size(px(11.5)).text_color(th.faint).child(if hidden { "Hidden" } else { "Shown" }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_calendar(c);
                    cx.notify();
                }))
        });
        let action = |id: &'static str, label: &'static str, key: &'static str| {
            row(id.into()).child(div().flex_1().child(label)).child(div().text_size(px(11.5)).font_weight(FontWeight::MEDIUM).text_color(th.faint).child(key))
        };
        Some(
            div()
                .id("menu-panel")
                .absolute()
                .top(px(TOPBAR_H - 6.))
                .right(px(20.))
                .w(px(260.))
                .opacity(t.clamp(0., 1.))
                .mt(px(-6. * (1. - t)))
                .p(px(6.))
                .flex()
                .flex_col()
                .gap(px(1.))
                .rounded(px(14.))
                .bg(th.surface)
                .border_1()
                .border_color(th.border)
                .shadow_lg()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .when(!secondary.is_empty(), |d| d.child(heading("More calendars")).children(calendars).child(div().my(px(4.)).h(px(1.)).bg(th.border)))
                .child(action("menu-edit", "Edit this view", "E").on_click(cx.listener(|this, _, _, cx| {
                    this.menu_on = false;
                    this.editor_on = true;
                    cx.notify();
                })))
                .child(action("menu-sync", "Sync now", "R").on_click(cx.listener(|this, _, _, cx| {
                    this.menu_on = false;
                    this.sync_now();
                    cx.notify();
                })))
                .child(action("menu-theme", if th.dark { "Light mode" } else { "Dark mode" }, "D").on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_theme();
                    cx.notify();
                })))
                .child(action("menu-help", "Keyboard shortcuts", "?").on_click(cx.listener(|this, _, _, cx| {
                    this.menu_on = false;
                    this.help_on = true;
                    cx.notify();
                }))),
        )
    }
}

/// Which calendars get chips, and how they're labelled; the rest go in the menu.
pub(super) struct ChipLayout {
    pub row: Vec<usize>,
    /// Chip labels, parallel to `row`; empty when `compact`.
    pub names: Vec<String>,
    /// Dots only.
    pub compact: bool,
    /// Secondary calendars, then any that don't fit even as dots.
    pub menu: Vec<usize>,
}

impl CalmView {
    /// Chips get what the title, clock and buttons leave: names first (long ones cut
    /// short), else just the dots, and whatever still doesn't fit moves to the menu.
    pub(super) fn chip_layout(&self) -> ChipLayout {
        let (title, subtitle) = self.title();
        let cut = |s: &str, n: usize| -> String { if s.chars().count() > n { s.chars().take(n - 1).chain(std::iter::once('…')).collect() } else { s.to_string() } };
        let status_len = self.status_line().map_or(0, |(t, _)| t.chars().count() + 3);
        let title_w = (title.chars().count() as f32 * 11.5).max((subtitle.chars().count() + status_len) as f32 * 6.6);
        // Measured: the clock column ~70, spacers 24, ‹ Today › and ☰ ~175, padding 56, gap 16.
        let clock_w = if self.viewport.0 < 640. { 50. } else { 70. };
        let room = self.viewport.0 - 56. - title_w - clock_w - 24. - 175. - 16.;
        let (mut row, mut menu): (Vec<usize>, Vec<usize>) = (0..self.calendars.len()).partition(|&c| !self.calendars[c].secondary);
        let width = |names: &[String]| -> f32 { names.iter().map(|n| 41. + n.chars().count() as f32 * 6.9).sum() };
        // Longest names that fit: up to 18 characters, then 10.
        for n in [18, 10] {
            let names: Vec<String> = row.iter().map(|&c| cut(self.calendars[c].display(), n)).collect();
            if width(&names) <= room {
                return ChipLayout { row, names, compact: false, menu };
            }
        }
        // Dots (34 px and a gap each); the rest join the menu, after the secondary ones.
        let fit = ((room / 40.).floor().max(0.) as usize).min(row.len());
        menu.extend(row.split_off(fit));
        ChipLayout { row, names: Vec::new(), compact: true, menu }
    }
}

impl CalmView {
    /// The top and bottom bars' own colour, when the theme gives them one (`chrome`).
    pub(super) fn chrome_bands(&self) -> Option<impl IntoElement + use<>> {
        let th = self.theme;
        if th.chrome.a < 0.002 {
            return None;
        }
        let bottom = (self.margin_b.x - 6.).max(0.);
        Some(
            div()
                .absolute()
                .size_full()
                .child(div().absolute().top_0().left_0().right_0().h(px(TOPBAR_H)).bg(th.chrome).border_b_1().border_color(th.border))
                .child(div().absolute().bottom_0().left_0().right_0().h(px(bottom)).bg(th.chrome).border_t_1().border_color(th.border)),
        )
    }
}

/// How the view's controls appear in the bottom row.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Params {
    None,
    Inline,
    /// One button opening them in a popup, labelled "Colour: Calendar", "Calendar" or "⋯".
    Button(u8),
}

/// Where the bottom row's pieces go: the view picker at the left (items `item` wide), the
/// view's controls centred at `params_x`, the zoom strip at the right (initials if `short`).
pub(super) struct BottomLayout {
    pub item: f32,
    pub short: bool,
    pub zoom_x: f32,
    pub params: Params,
    pub params_x: f32,
}

impl CalmView {
    /// Fits the bottom row to the window. Giving way in order: zoom labels shorten,
    /// picker items narrow, then the controls fold into one button (and narrow again).
    pub(super) fn bottom_layout(&self) -> BottomLayout {
        let vw = self.viewport.0;
        let (n, stops) = (self.views.len() as f32, self.spec().stops.len() as f32);
        let (margin, gap) = (16., 16.);
        let picker = |item: f32| n * item + 10.;
        let zoom = |short: bool| stops * if short { 26. } else { 58. } + 8.;
        let inline_w = self.params_width();
        let button_w = |tier: u8| 32. + self.params_label(tier).chars().count() as f32 * 6.9;
        let mode_w = |m: Params| match m {
            Params::None => 0.,
            Params::Inline => inline_w,
            Params::Button(t) => button_w(t),
        };
        // The controls sit centred in the window if they can, else as near it as the slot allows.
        let place = |item: f32, short: bool, mode: Params| -> Option<BottomLayout> {
            let (lo, hi) = (margin + picker(item) + gap, vw - margin - zoom(short) - gap);
            let w = mode_w(mode);
            (hi - lo >= w).then(|| BottomLayout { item, short, zoom_x: vw - margin - zoom(short), params: mode, params_x: ((vw - w) * 0.5).clamp(lo, (hi - w).max(lo)) })
        };
        let shrunk = |short: bool, mode: Params| -> f32 {
            // Picker items as wide as the slot allows, between 48 and full size.
            // Readable names (48 px) before anything else gives way; narrower only as a last resort.
            ((vw - 2. * margin - zoom(short) - 2. * gap - mode_w(mode) - 10.) / n).clamp(48., SWITCH_W)
        };
        let modes: &[Params] = if inline_w == 0. { &[Params::None] } else { &[Params::Inline, Params::Button(0), Params::Button(1), Params::Button(2)] };
        for &mode in modes {
            for (item, short) in [(SWITCH_W, false), (SWITCH_W, true), (shrunk(true, mode), true)] {
                if let Some(l) = place(item, short, mode) {
                    return l;
                }
            }
        }
        // Narrower still: tighter gaps, and picker items down to two letters.
        let mode = *modes.last().unwrap();
        let tight = 8.;
        let item = ((vw - 2. * margin - zoom(true) - 2. * tight - mode_w(mode) - 10.) / n).clamp(34., SWITCH_W);
        BottomLayout { item, short: true, zoom_x: vw - margin - zoom(true), params: mode, params_x: margin + picker(item) + tight }
    }
}
