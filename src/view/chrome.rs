//! Window chrome built from gpui elements: top bar, lens switcher, tooltip,
//! detail drawer, help. Positions and opacities come from our springs.

use std::time::Duration;

use chrono::Datelike;
use gpui::{
    Animation, AnimationExt, Context, FontWeight, Hsla, SharedString, Stateful, Div, div, ease_out_quint,
    prelude::*, px,
};

use super::scene::{DAY_NAMES, fmt_range};
use super::{CalmView, Stats};
use crate::color::{self, ColorEnc, Compose, Measure};
use crate::model::{CALENDARS, Status};
use crate::spec::{Detail, PRESETS};
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
        let key = hash(&title) ^ hash(&subtitle);
        let compact = self.viewport.0 < 940.;
        let chips = (0..CALENDARS.len()).map(|c| {
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
                .when(!compact, |d| d.child(CALENDARS[c]))
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
                    .child(div().text_size(px(12.5)).text_color(th.faint).child(subtitle))
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
                    .children(chips)
                    .child(div().w(px(14.)))
                    .when(can_pan, |d| {
                        d.child(self.button("prev", "‹").text_size(px(18.)).on_click(cx.listener(|this, _, _, cx| {
                            this.pan(-1);
                            cx.notify();
                        })))
                        .child(self.button("today", "Today").on_click(cx.listener(|this, _, _, cx| {
                            this.go_today();
                            cx.notify();
                        })))
                        .child(self.button("next", "›").text_size(px(18.)).on_click(cx.listener(|this, _, _, cx| {
                            this.pan(1);
                            cx.notify();
                        })))
                    }),
            )
    }

    fn chip(&self, id: impl Into<gpui::ElementId>, label: &'static str, active: bool) -> Stateful<Div> {
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
            .child(label)
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

    /// The colour channel of the grammar, editable per lens.
    fn color_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let th = self.theme;
        let enc = self.color();
        let aggregate = self.aggregate();
        let caption = |s: &'static str| div().px(px(8.)).text_size(px(11.)).text_color(th.faint).child(s);

        let mut row = self.bar().child(caption("Colour"));
        if aggregate {
            for (i, m) in Measure::ALL.into_iter().enumerate() {
                row = row.child(self.chip(("measure", i), m.label(), enc.measure == m).on_click(on_color(cx, move |e| e.measure = m)));
            }
            row = row.child(div().w(px(1.)).h(px(16.)).mx(px(4.)).bg(th.border));
            let facetable = enc.measure.facetable();
            row = row.child(
                self.chip("by-cal", "By calendar", enc.faceted())
                    .when(!facetable, |d| d.opacity(0.4))
                    .on_click(on_color(cx, |e| e.by_calendar = !e.by_calendar)),
            );
        } else {
            row = row.child(self.chip("m-cal", "Calendar", enc.by_calendar).on_click(on_color(cx, |e| e.by_calendar = true)));
            for (i, (m, label)) in [(Measure::Hours, "Duration"), (Measure::Demand, "Demand"), (Measure::Strain, "Strain")].into_iter().enumerate() {
                let active = !enc.by_calendar && enc.measure == m;
                row = row.child(self.chip(("m-ev", i), label, active).on_click(on_color(cx, move |e| {
                    e.by_calendar = false;
                    e.measure = m;
                })));
            }
        }
        // Legend for continuous scales; calendar colours are legended by the top-bar chips.
        let legend = (!enc.faceted() && (aggregate || enc.measure.per_event())).then(|| {
            let (lo, hi) = enc.measure.legend();
            let swatch = if enc.measure.diverging_per_event() && !aggregate {
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
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(8.))
                .text_size(px(10.5))
                .text_color(th.faint)
                .child(lo)
                .child(swatch.border_1().border_color(th.border))
                .child(hi)
        });

        let (legend_top, legend_row) = if aggregate { (legend, None) } else { (None, legend) };
        // Aggregates get a second row: how facets compose (or the legend), and nested glyphs.
        let extra = aggregate.then(|| {
            let mut top = self.bar();
            if enc.faceted() {
                top = top.child(caption("Compose"));
                for (i, c) in Compose::ALL.into_iter().enumerate() {
                    top = top.child(self.chip(("compose", i), c.label(), enc.compose == c).on_click(on_color(cx, move |e| e.compose = c)));
                }
            } else {
                top = top.children(legend_top);
            }
            let span_on = self.details[self.preset] == Detail::DaySpan;
            top.child(div().w(px(1.)).h(px(16.)).mx(px(4.)).bg(th.border))
                .child(self.chip("day-span", "Day span", span_on).on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_detail();
                    cx.notify();
                })))
                .with_animation(
                    ("extra-in", enc.faceted() as usize),
                    Animation::new(Duration::from_millis(320)).with_easing(ease_out_quint()),
                    |el, t| el.opacity(t).mb(px(-6. * (1. - t))),
                )
        });
        let row = row.children(legend_row);

        div().flex().flex_col().items_center().gap(px(6.)).children(extra).child(row)
    }

    pub(super) fn switcher(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let th = self.theme;
        let color_bar = self.color_bar(cx);
        let items = PRESETS.iter().enumerate().map(|(i, p)| {
            let active = i == self.preset;
            div()
                .id(("lens", i))
                .w(px(SWITCH_W))
                .h(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .text_size(px(12.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if active { th.ink } else { th.faint })
                .cursor_pointer()
                .hover(move |s| s.text_color(th.ink))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_preset(i);
                    cx.notify();
                }))
                .child(p.name)
        });
        div()
            .absolute()
            .bottom(px(18.))
            .left_0()
            .right_0()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.))
            .child(color_bar)
            .child(
                div()
                    .relative()
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
                            .left(px(4. + self.switcher.x * SWITCH_W))
                            .w(px(SWITCH_W))
                            .h(px(30.))
                            .rounded_full()
                            .bg(with_alpha(th.ink, if th.dark { 0.12 } else { 0.07 })),
                    )
                    .children(items),
            )
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
            let when = format!("{} {} · {}", DAY_NAMES[e.day.rem_euclid(7) as usize], date.day(), fmt_range(e.start_min, e.end_min()));
            let strained = self.preset == 2 && crate::model::strain(e) >= crate::model::STRAIN;
            let line2 = if strained { Some("Above your usual energy at this hour".into()) } else { e.location.clone() };
            (e.title.clone(), when, line2, th.cals[e.calendar].dot)
        } else {
            let day = self.hover_day?;
            let s = self.cells.get(day as usize)?;
            let date = self.data.date(day);
            let title = date.format("%A %-d %B").to_string().into();
            let visible: Vec<usize> = (0..CALENDARS.len()).filter(|&c| !self.hidden[c]).collect();
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
            let breakdown = parts.iter().map(|&(c, v)| format!("{} {}", CALENDARS[c], fmt(v))).collect::<Vec<_>>().join(" · ");
            let dot = parts.first().map_or(th.cell, |&(c, _)| th.cals[c].dot);
            let span = s.bounds(&self.hidden).map(|(a, b)| format!("{} – {} · {:.1}h span", super::scene::fmt_time(a), super::scene::fmt_time(b), (b - a) / 60.));
            let timing = matches!(m, Measure::Starts | Measure::Ends | Measure::Span) || self.details[self.preset] == Detail::DaySpan;
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
                                        .child(div().text_size(px(12.)).text_color(th.muted).child(CALENDARS[e.calendar]))
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
                    let yours = crate::model::personal_energy((e.start_min + e.end_min()) * 0.5);
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
        let keys: [(&str, &str); 10] = [
            ("1 – 5, Tab", "Switch lens"),
            ("← →, Shift+scroll", "Previous / next"),
            ("↑ ↓, scroll", "Scroll hours"),
            ("Ctrl+scroll, + −", "Zoom hours"),
            ("T", "Jump to today"),
            ("F", "Fisheye around the pointer"),
            ("D", "Light / dark"),
            ("Click", "Event details · open a week"),
            ("Esc", "Close"),
            ("`", "Frame stats"),
        ];
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
                        .children(keys.into_iter().map(|(k, v)| {
                            div()
                                .flex()
                                .justify_between()
                                .text_size(px(13.))
                                .child(div().text_color(th.muted).child(v))
                                .child(div().font_weight(FontWeight::MEDIUM).child(k))
                        })),
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

/// Click handler that edits the current lens's colour encoding.
fn on_color(
    cx: &mut Context<CalmView>,
    f: impl Fn(&mut ColorEnc) + 'static,
) -> impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static {
    cx.listener(move |this, _, _, cx| {
        let mut enc = this.color();
        f(&mut enc);
        this.set_color(enc);
        cx.notify();
    })
}

impl CalmView {
    /// Zoom stops on the right edge; the highlight follows the continuous zoom.
    pub(super) fn zoom_slider(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        const ITEM: f32 = 26.;
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
        let items = spec.stops.iter().enumerate().map(|(i, s)| {
            div()
                .id(("stop", i))
                .h(px(ITEM))
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
                .child(s.name)
        });
        div()
            .absolute()
            .right(px(6.))
            .top(px(self.plot().1))
            .w(px(super::scene::MARGIN_R - 12.))
            .p(px(3.))
            .flex()
            .flex_col()
            .rounded(px(16.))
            .bg(th.surface)
            .border_1()
            .border_color(th.border)
            .shadow_sm()
            .child(
                div()
                    .absolute()
                    .top(px(3. + pos * ITEM))
                    .left(px(3.))
                    .right(px(3.))
                    .h(px(ITEM))
                    .rounded_full()
                    .bg(with_alpha(th.ink, if th.dark { 0.12 } else { 0.07 })),
            )
            .children(items)
    }
}
