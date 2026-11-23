//! Editing in the app: the details drawer's fields, delete with undo, and what a queued
//! change needs from you. Every edit becomes an outbox change (see `docs/editing.md`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Datelike, Local, NaiveDate, TimeZone, Timelike, Utc};
use gpui::{Context, Div, Entity, FontWeight, ScrollHandle, Subscription, Window, div, point, prelude::*, px};
use serde_json::{Value, json};

use super::CalmView;
use super::input::{InputEvent, InputStyle, TextInput};
use crate::model::EditRef;
use crate::sync::edit;
use crate::sync::outbox::{Origin, State};
use crate::theme::with_alpha;

/// The drawer's text fields, for the event it shows.
pub(super) struct DrawerEdit {
    /// Which event: its store key and ID.
    event: (String, String),
    pub title: Entity<TextInput>,
    pub location: Entity<TextInput>,
    _subs: Vec<Subscription>,
}

#[derive(Clone, Copy)]
enum Field {
    Title,
    Location,
}

/// The open date or time picker in the drawer.
pub(super) enum Picker {
    /// A month grid, showing the month containing this date.
    Date(NaiveDate),
    Start(ScrollHandle),
    End(ScrollHandle),
}

/// Height of a row in the time lists.
const TIME_ROW: f32 = 28.;

/// A note at the bottom of the window, with an undo.
pub(super) struct Toast {
    pub text: String,
    pub undo: Option<Undo>,
    pub until: Instant,
}

pub(super) enum Undo {
    /// Drop the delete if it hasn't gone out yet; otherwise bring the event back as a new one
    /// from its fields.
    Delete { seq: Option<i64>, key: String, fields: Value },
    /// Move the event back (which, unsent, cancels the change).
    Reschedule { key: String, id: String, from: (i64, i64) },
}

impl CalmView {
    /// The event the drawer shows, if you may edit it.
    fn editable_shown(&self) -> Option<(usize, Arc<EditRef>)> {
        let i = self.shown?;
        let r = self.data.events.get(i)?.edit.clone()?;
        r.editable.then_some((i, r))
    }

    /// Keeps the drawer's fields in step with the event it shows.
    pub(super) fn sync_drawer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((i, r)) = self.editable_shown() else {
            self.drawer_edit = None;
            return;
        };
        let key = (r.key.clone(), r.id.clone());
        if self.drawer_edit.as_ref().is_some_and(|d| d.event == key) {
            return;
        }
        let th = self.theme;
        let style = |size: f32| InputStyle { text: th.ink, placeholder: th.faint, cursor: th.now, selection: with_alpha(th.now, 0.25), size };
        let e = &self.data.events[i];
        let make = |text: String, placeholder: &str, size: f32, cx: &mut Context<Self>| {
            let p = placeholder.to_string();
            cx.new(|cx| TextInput::new(text, p, style(size), cx))
        };
        let title = make(e.title.to_string(), "Title", 21., cx);
        let location = make(e.location.as_ref().map(|l| l.to_string()).unwrap_or_default(), "Add a location", 13., cx);
        let mut subs = Vec::new();
        for (input, field) in [(&title, Field::Title), (&location, Field::Location)] {
            subs.push(cx.subscribe_in(input, window, move |this, _, ev: &InputEvent, window, cx| {
                match ev {
                    InputEvent::Submit => {
                        this.commit_field(field, cx);
                        window.focus(&this.focus);
                    }
                    InputEvent::Blur => this.commit_field(field, cx),
                    InputEvent::Cancel => {
                        this.drawer_edit = None;
                        window.focus(&this.focus);
                    }
                }
                cx.notify();
            }));
        }
        self.drawer_edit = Some(DrawerEdit { event: key, title, location, _subs: subs });
        self.picker = None;
        if let Some(n) = self.pending_picker.take() {
            self.open_picker(n, cx);
        }
        if std::mem::take(&mut self.focus_title)
            && let Some(d) = &self.drawer_edit
        {
            d.title.update(cx, |t, cx| t.select_everything(cx));
            d.title.read(cx).focus(window);
        }
    }

    /// Whether you're typing in one of the drawer's fields (shortcuts then step aside).
    pub(super) fn typing(&self, window: &Window, cx: &gpui::App) -> bool {
        self.drawer_edit
            .as_ref()
            .is_some_and(|d| [&d.title, &d.location].iter().any(|i| i.read(cx).is_focused(window)))
    }

    /// Turns a field's new text into a change (if it changed).
    fn commit_field(&mut self, field: Field, cx: &mut Context<Self>) {
        let Some((i, r)) = self.editable_shown() else { return };
        let Some(d) = &self.drawer_edit else { return };
        let e = &self.data.events[i];
        let now = edit::fields(&r, &e.title, e.location.as_ref().map(|l| l.as_ref()));
        let text = |input: &Entity<TextInput>| input.read(cx).text().trim().to_string();
        // Compared as the field shows them (one line), so an untouched multi-line value isn't
        // rewritten.
        let shown = |s: &str| crate::text::one_line(s);
        let patch = match field {
            Field::Title => {
                let t = text(&d.title);
                (!t.is_empty() && t != shown(&e.title)).then(|| json!({ "summary": t }))
            }
            Field::Location => {
                let l = text(&d.location);
                (l != shown(e.location.as_ref().map_or("", |s| s.as_ref()))).then(|| json!({ "location": l }))
            }
        };
        let Some(patch) = patch else { return };
        let mut op = edit::update(&r, &now, patch, Origin::App, State::Pending);
        op.notify = self.notify_choice(&r);
        self.queue_change(op, cx);
    }

    /// Guests hear about changes unless you turned it off for this event.
    pub(super) fn notify_choice(&self, r: &EditRef) -> String {
        if r.guests > 0 && !self.quiet.contains(&(r.key.clone(), r.id.clone())) { "all".into() } else { "none".into() }
    }

    /// Deletes the drawer's (or selected) event, with an undo.
    pub(super) fn delete_selected(&mut self, cx: &mut Context<Self>) {
        let Some(i) = self.selected.or(self.shown) else { return };
        let Some(e) = self.data.events.get(i) else { return };
        let Some(r) = e.edit.clone().filter(|r| r.editable) else {
            self.toast(format!("{} is read-only", e.title), None, cx);
            return;
        };
        let fields = edit::fields(&r, &e.title, e.location.as_ref().map(|l| l.as_ref()));
        let title = e.title.to_string();
        let mut op = edit::delete(&r, &fields, Origin::App, State::Pending);
        op.notify = self.notify_choice(&r);
        let seq = self.queue_change(op, cx);
        self.selected = None;
        self.drawer_edit = None;
        let undo = Undo::Delete { seq, key: r.key.clone(), fields };
        self.toast(format!("Deleted \u{201c}{title}\u{201d}"), Some(undo), cx);
    }

    /// Shows a note for a few seconds.
    pub(super) fn toast(&mut self, text: String, undo: Option<Undo>, cx: &mut Context<Self>) {
        let until = Instant::now() + Duration::from_secs(8);
        self.toast = Some(Toast { text, undo, until });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(8100)).await;
            let _ = this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|t| t.until <= Instant::now()) {
                    this.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Undoes what the toast offers: drops the change if it hasn't gone out, else reverses it.
    fn undo_toast(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.toast.take() else { return };
        let Ok(store) = crate::sync::store::Store::open(&crate::sync::db_path()) else { return };
        if let Some(Undo::Reschedule { key, id, from }) = &t.undo {
            self.reschedule(key, id, *from, false, cx);
        }
        if let Some(Undo::Delete { seq, key, fields }) = t.undo {
            // Not sent yet: just drop it. (A delete of an unsent new event left nothing to drop:
            // that one never existed anywhere else, so it comes back as new too.)
            let dropped = seq.is_some_and(|s| store.discard(&[s]).unwrap_or(0) == 1);
            if dropped {
                self.reload_live();
            } else {
                let mut fields = fields;
                if let Some(o) = fields.as_object_mut() {
                    o.retain(|k, _| ["summary", "location", "description", "start", "end"].contains(&k.as_str()));
                }
                let op = edit::create(&key, fields, Origin::App, State::Pending);
                self.queue_change(op, cx);
            }
        }
        cx.notify();
    }

    /// The toast, above the bottom row.
    pub(super) fn toast_card(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let t = self.toast.as_ref()?;
        let th = self.theme;
        Some(
            div().absolute().bottom(px(self.margin_b.x + 10.)).left_0().right_0().flex().justify_center().child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .px(px(16.))
                    .py(px(8.))
                    .rounded_full()
                    .bg(th.ink)
                    .text_color(th.bg)
                    .text_size(px(13.))
                    .shadow_lg()
                    .child(t.text.clone())
                    .when(t.undo.is_some(), |d| {
                        d.child(
                            div()
                                .id("toast-undo")
                                .font_weight(FontWeight::SEMIBOLD)
                                .cursor_pointer()
                                .child("Undo")
                                .on_click(cx.listener(|this, _, _, cx| this.undo_toast(cx))),
                        )
                    }),
            ),
        )
    }

    /// The drawer's editing extras for an editable event: guests, delete, and what its queued
    /// change needs from you.
    pub(super) fn drawer_actions(&self, cx: &mut Context<Self>) -> Option<Div> {
        let (_, r) = self.editable_shown()?;
        let th = self.theme;
        let op = crate::sync::store::Store::open(&crate::sync::db_path())
            .ok()
            .and_then(|s| s.ops().ok())
            .and_then(|ops| ops.into_iter().rev().find(|o| o.key() == r.key && o.event == r.id));
        let note = |text: String, color| div().text_size(px(12.)).text_color(color).child(text);
        let mut d = div().flex().flex_col().gap(px(10.));
        if let Some(op) = &op {
            let seq = op.seq;
            let (text, color): (String, _) = match op.state {
                State::Draft => ("Staged: not sent until you commit".into(), th.muted),
                State::Pending | State::Sending => (op.last_error.clone().map_or("Waiting to sync".into(), |e| format!("Waiting to sync · {e}")), th.muted),
                State::Failed => (op.last_error.clone().unwrap_or_else(|| "Couldn't sync".into()), th.now),
                State::Conflict => ("Changed elsewhere since you edited it".into(), th.now),
            };
            d = d.child(note(text, color));
            let mut row = div().flex().gap(px(6.));
            match op.state {
                State::Draft => {
                    row = row
                        .child(self.button("op-commit", "Commit").on_click(cx.listener(move |this, _, _, cx| this.commit_changes(&[seq], cx))))
                        .child(self.button("op-revert", "Revert").on_click(cx.listener(move |this, _, _, cx| this.discard_changes(&[seq], cx))));
                }
                State::Failed => {
                    row = row
                        .child(self.button("op-retry", "Retry").on_click(cx.listener(move |this, _, _, cx| this.commit_changes(&[seq], cx))))
                        .child(self.button("op-discard", "Discard").on_click(cx.listener(move |this, _, _, cx| this.discard_changes(&[seq], cx))));
                }
                State::Conflict => {
                    if let Some(theirs) = &op.theirs {
                        d = d.child(note(format!("Theirs: {}", summarise(theirs)), th.faint)).child(note(format!("Yours: {}", summarise(&op.patch)), th.faint));
                    }
                    row = row
                        .child(self.button("op-mine", "Keep mine").on_click(cx.listener(move |this, _, _, cx| this.keep_mine(seq, cx))))
                        .child(self.button("op-theirs", "Keep theirs").on_click(cx.listener(move |this, _, _, cx| this.discard_changes(&[seq], cx))));
                }
                _ => {}
            }
            d = d.child(row);
        }
        let mut row = div().flex().items_center().gap(px(6.));
        if r.guests > 0 {
            let quiet = self.quiet.contains(&(r.key.clone(), r.id.clone()));
            let key = (r.key.clone(), r.id.clone());
            row = row.child(
                self.chip("notify", format!("Notify {} guest{}", r.guests, if r.guests == 1 { "" } else { "s" }), !quiet).on_click(cx.listener(move |this, _, _, cx| {
                    if !this.quiet.remove(&key) {
                        this.quiet.insert(key.clone());
                    }
                    cx.notify();
                })),
            );
        }
        row = row.child(div().flex_1()).child(
            self.button("delete-event", "Delete").text_color(th.now).on_click(cx.listener(|this, _, _, cx| this.delete_selected(cx))),
        );
        Some(d.child(row))
    }

    pub(super) fn commit_changes(&mut self, seqs: &[i64], cx: &mut Context<Self>) {
        if let Ok(store) = crate::sync::store::Store::open(&crate::sync::db_path()) {
            let _ = store.commit(seqs);
        }
        self.reload_live();
        if let Some(s) = &self.sync {
            let _ = s.commands.send(crate::sync::Command::Flush);
        }
        cx.notify();
    }

    pub(super) fn discard_changes(&mut self, seqs: &[i64], cx: &mut Context<Self>) {
        if let Ok(store) = crate::sync::store::Store::open(&crate::sync::db_path()) {
            let _ = store.discard(seqs);
        }
        self.drawer_edit = None;
        self.reload_live();
        cx.notify();
    }

    /// Resolves a conflict your way: your values, sent against their latest version.
    pub(super) fn keep_mine(&mut self, seq: i64, cx: &mut Context<Self>) {
        if let Ok(store) = crate::sync::store::Store::open(&crate::sync::db_path()) {
            let _ = store.keep_mine(seq);
        }
        self.commit_changes(&[seq], cx);
    }
}

/// A change's fields in a few words.
fn summarise(v: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(s) = v["start"]["dateTime"].as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) {
        let e = v["end"]["dateTime"].as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
        let (s, e) = (s.with_timezone(&Local), e.map(|e| e.with_timezone(&Local)));
        parts.push(format!("{}{}", s.format("%a %-d %H:%M"), e.map_or(String::new(), |e| format!("–{}", e.format("%H:%M")))));
    }
    for k in ["summary", "location"] {
        if let Some(t) = v[k].as_str() {
            parts.push(format!("\u{201c}{t}\u{201d}"));
        }
    }
    if v["deleted"].as_bool() == Some(true) {
        parts.push("deleted".into());
    }
    parts.join(" · ")
}

/// What a change does, in two short lines: "Move · Standup" and "Tue 09:00–09:15 → 10:00–10:15".
fn change_lines(op: &crate::sync::outbox::Op) -> (String, String) {
    use crate::sync::outbox::Kind;
    let title = op.patch["summary"].as_str().or(op.base["summary"].as_str()).unwrap_or("(untitled)").to_string();
    match op.kind {
        Kind::Create => (format!("New · {title}"), summarise(&op.patch)),
        Kind::Delete => (format!("Delete · {title}"), summarise(&op.base)),
        Kind::Update => {
            let what = if op.patch.get("start").is_some() { "Move" } else { "Edit" };
            let mut before = op.base.clone();
            if let Some(b) = before.as_object_mut() {
                b.retain(|k, _| op.patch.get(k).is_some());
            }
            (format!("{what} · {title}"), format!("{} → {}", summarise(&before), summarise(&op.patch)))
        }
    }
}

impl CalmView {
    /// The changes panel: everything queued, what you can do about it, and commit.
    pub(super) fn changes_panel(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if !self.changes_on {
            return None;
        }
        let th = self.theme;
        let ops = crate::sync::store::Store::open(&crate::sync::db_path()).ok().and_then(|s| s.ops().ok()).unwrap_or_default();
        let committable = |o: &crate::sync::outbox::Op| matches!(o.state, State::Draft | State::Failed | State::Conflict);
        let ticked: Vec<i64> = ops.iter().filter(|o| committable(o) && !self.changes_unticked.contains(&o.seq)).map(|o| o.seq).collect();
        let review = self.conf.settings.writes.as_deref() == Some("review");
        let rows = ops.iter().map(|o| {
            let seq = o.seq;
            let (line1, line2) = change_lines(o);
            let (badge, color) = match o.state {
                State::Draft => ("staged", th.muted),
                State::Pending => ("waiting", th.muted),
                State::Sending => ("sending", th.muted),
                State::Failed => ("failed", th.now),
                State::Conflict => ("conflict", th.now),
            };
            let tick = committable(o).then(|| {
                let on = !self.changes_unticked.contains(&seq);
                div()
                    .id(gpui::ElementId::NamedInteger("tick".into(), seq as u64))
                    .size(px(15.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(if on { th.ink } else { th.faint })
                    .when(on, |d| d.bg(with_alpha(th.ink, 0.85)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.changes_unticked.remove(&seq) {
                            this.changes_unticked.insert(seq);
                        }
                        cx.notify();
                    }))
            });
            let mut actions = div().flex().gap(px(4.));
            if o.state == State::Conflict {
                actions = actions
                    .child(self.button(gpui::ElementId::NamedInteger("mine".into(), seq as u64), "Keep mine").on_click(cx.listener(move |this, _, _, cx| this.keep_mine(seq, cx))))
                    .child(self.button(gpui::ElementId::NamedInteger("theirs".into(), seq as u64), "Keep theirs").on_click(cx.listener(move |this, _, _, cx| this.discard_changes(&[seq], cx))));
            } else if o.state != State::Sending {
                actions = actions.child(self.button(gpui::ElementId::NamedInteger("revert".into(), seq as u64), "Revert").on_click(cx.listener(move |this, _, _, cx| this.discard_changes(&[seq], cx))));
            }
            div()
                .flex()
                .items_start()
                .gap(px(10.))
                .py(px(8.))
                .border_b_1()
                .border_color(th.border)
                .child(div().pt(px(2.)).w(px(15.)).children(tick))
                .child(
                    // The text gives way (clipped) before the buttons do.
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap(px(8.))
                                .items_center()
                                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(line1))
                                .child(div().text_size(px(11.)).text_color(color).child(badge))
                                .when(o.origin == Origin::Cli, |d| d.child(div().text_size(px(11.)).text_color(th.faint).child("from agent"))),
                        )
                        .child(div().text_size(px(12.)).text_color(th.muted).child(line2))
                        .children(o.theirs.as_ref().map(|t| div().text_size(px(12.)).text_color(th.now).child(format!("Elsewhere: {}", summarise(t)))))
                        .children(o.last_error.as_ref().map(|e| div().text_size(px(12.)).text_color(if o.state == State::Failed { th.now } else { th.faint }).child(e.clone()))),
                )
                .child(actions.flex_none())
        });
        let n = ticked.len();
        let preview = PREVIEW_ON.load(std::sync::atomic::Ordering::Relaxed);
        let to_commit = ticked.clone();
        let to_discard = ticked.clone();
        Some(
            div()
                .id("changes")
                .absolute()
                .top(px(76.))
                .right(px(16.))
                .w(px(420.))
                .max_h(px((self.viewport.1 - 76. - 90.).max(200.)))
                .overflow_y_scroll()
                .p(px(18.))
                .flex()
                .flex_col()
                .gap(px(8.))
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
                        .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child("Changes"))
                        .child(self.button("changes-close", "✕").text_size(px(13.)).on_click(cx.listener(|this, _, _, cx| {
                            this.changes_on = false;
                            cx.notify();
                        }))),
                )
                .child(div().text_size(px(12.)).text_color(th.faint).child(if review {
                    "Review mode: your changes wait here until you commit them."
                } else {
                    "Your changes send by themselves; ones from agents wait here for you."
                }))
                .when(ops.is_empty(), |d| d.child(div().py(px(12.)).text_size(px(13.)).text_color(th.muted).child("Nothing waiting.")))
                .children(rows)
                .when(!ops.is_empty(), |d| {
                    d.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .mt(px(6.))
                            .child(self.button("commit-ticked", format!("Commit {n}")).when(n > 0, |b| b.text_color(th.ink)).on_click(cx.listener(move |this, _, _, cx| {
                                this.commit_changes(&to_commit, cx);
                            })))
                            .child(self.button("discard-ticked", "Discard").on_click(cx.listener(move |this, _, _, cx| {
                                this.discard_changes(&to_discard, cx);
                            })))
                            .child(div().flex_1())
                            .child(self.chip("preview", "Preview", preview).on_click(cx.listener(|this, _, _, cx| {
                                let on = !PREVIEW_ON.load(std::sync::atomic::Ordering::Relaxed);
                                PREVIEW_ON.store(on, std::sync::atomic::Ordering::Relaxed);
                                this.reload_live();
                                cx.notify();
                            }))),
                    )
                }),
        )
    }

    /// A new event from `start` to `end` (Google's shape) in your default calendar; opened in
    /// the drawer with its title ready to type.
    pub(super) fn add_event(&mut self, start: Value, end: Value, cx: &mut Context<Self>) {
        let Some(key) = self.default_calendar() else {
            self.toast("No calendar you can add to: connect a Google account (docs/google.md)".into(), None, cx);
            return;
        };
        let fields = json!({ "summary": "New event", "start": start, "end": end });
        let op = edit::create(&key, fields, Origin::App, State::Pending);
        let id = op.event.clone();
        self.queue_change(op, cx);
        // Select it and put you in its title.
        if let Some(i) = self.data.events.iter().position(|e| e.edit.as_ref().is_some_and(|r| r.id == id)) {
            self.selected = Some(i);
            self.shown = Some(i);
            self.focus_title = true;
        }
        cx.notify();
    }

    /// `default-calendar`, else your primary (named after its account), else the first you can write.
    fn default_calendar(&self) -> Option<String> {
        let store = crate::sync::store::Store::open(&crate::sync::db_path()).ok()?;
        let sources = self.sync.as_ref()?.sources.clone();
        let want = self.conf.settings.default_calendar.clone();
        let mut first = None;
        for s in sources.iter().filter(|s| matches!(s.location, crate::sync::Location::Google(_))) {
            for c in store.calendars(&s.name).ok()?.into_iter().filter(|c| c.access == "owner" || c.access == "writer") {
                let key = format!("{}/{}", s.name, c.id);
                let label = self.calendars.iter().find(|k| k.name == c.name).map(|k| k.display().to_string());
                if want.as_deref().is_some_and(|w| w.eq_ignore_ascii_case(&c.name) || label.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(w))) {
                    return Some(key);
                }
                if want.is_none() && c.name == s.name {
                    return Some(key);
                }
                first.get_or_insert(key);
            }
        }
        first
    }
}

use super::PREVIEW as PREVIEW_ON;

impl CalmView {
    /// Moves the drawer's event to `start`–`end` (as a change).
    fn set_times(&mut self, start: chrono::DateTime<Utc>, end: chrono::DateTime<Utc>, cx: &mut Context<Self>) {
        self.picker = None;
        let Some((i, r)) = self.editable_shown() else { return };
        if start == r.start && end == r.end {
            return;
        }
        let e = &self.data.events[i];
        let now = edit::fields(&r, &e.title, e.location.as_ref().map(|l| l.as_ref()));
        // All-day events move as dates (stored at UTC midnight); timed ones as times.
        let patch = if r.all_day {
            json!({ "start": edit::date(start.date_naive()), "end": edit::date(end.date_naive()) })
        } else {
            json!({ "start": edit::time(start), "end": edit::time(end) })
        };
        let mut op = edit::update(&r, &now, patch, Origin::App, State::Pending);
        op.notify = self.notify_choice(&r);
        self.queue_change(op, cx);
        cx.notify();
    }

    /// Opens a picker (or closes it, if it's the one open), scrolled to the current choice.
    fn open_picker(&mut self, which: u8, cx: &mut Context<Self>) {
        let Some((_, r)) = self.editable_shown() else { return };
        let same = matches!((&self.picker, which), (Some(Picker::Date(_)), 0) | (Some(Picker::Start(_)), 1) | (Some(Picker::End(_)), 2));
        if same {
            self.picker = None;
        } else {
            let start = r.start.with_timezone(&Local);
            self.picker = Some(match which {
                0 => Picker::Date(day_of(&r)),
                n => {
                    let handle = ScrollHandle::new();
                    // Start: the current time a few rows down; End: the list starts at the current end.
                    let row = if n == 1 { (start.hour() * 4 + start.minute().div_ceil(15)) as f32 } else { ((r.end - r.start).num_minutes() as f32 / 15.).max(1.) - 1. };
                    handle.set_offset(point(px(0.), px(-(row - 3.).max(0.) * TIME_ROW)));
                    if n == 1 { Picker::Start(handle) } else { Picker::End(handle) }
                }
            });
        }
        cx.notify();
    }

    /// The drawer's date and times: buttons that open a month grid and time lists.
    pub(super) fn time_row(&self, cx: &mut Context<Self>) -> Option<Div> {
        let (_, r) = self.editable_shown()?;
        let th = self.theme;
        let (s, e) = (r.start.with_timezone(&Local), r.end.with_timezone(&Local));
        let field = |id: &'static str, label: String, open: bool| {
            div()
                .id(id)
                .px(px(10.))
                .py(px(4.))
                .rounded(px(7.))
                .border_1()
                .border_color(if open { th.ink } else { th.border })
                .bg(th.bg)
                .text_size(px(13.))
                .text_color(th.ink)
                .cursor_pointer()
                .hover(move |d| d.border_color(th.muted))
                .child(label)
        };
        let open = |n: u8| matches!((&self.picker, n), (Some(Picker::Date(_)), 0) | (Some(Picker::Start(_)), 1) | (Some(Picker::End(_)), 2));
        let buttons = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_color(th.muted)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(field("pick-date", day_of(&r).format("%a %-d %b").to_string(), open(0)).on_click(cx.listener(|this, _, _, cx| this.open_picker(0, cx))))
            .when(!r.all_day, |d| {
                d.child(field("pick-start", s.format("%H:%M").to_string(), open(1)).on_click(cx.listener(|this, _, _, cx| this.open_picker(1, cx))))
                    .child("–")
                    .child(field("pick-end", e.format("%H:%M").to_string(), open(2)).on_click(cx.listener(|this, _, _, cx| this.open_picker(2, cx))))
            });
        let popover = match &self.picker {
            None => None,
            Some(Picker::Date(month)) => Some(self.month_grid(*month, &r, cx).into_any_element()),
            Some(Picker::Start(h)) => Some(self.time_list(h, &r, true, cx).into_any_element()),
            Some(Picker::End(h)) => Some(self.time_list(h, &r, false, cx).into_any_element()),
        };
        // Deferred: painted after the rest of the drawer, so it sits on top.
        Some(div().relative().child(buttons).children(popover.map(|p| {
            gpui::deferred(
            div()
                .absolute()
                .top(px(34.))
                .left_0()
                .p(px(6.))
                .rounded(px(12.))
                .bg(th.surface)
                .border_1()
                .border_color(th.border)
                .shadow_lg()
                .child(p)
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()),
            )
            .with_priority(1)
        })))
    }

    /// Times every 15 minutes: from midnight for the start (the event keeps its length), or
    /// after the start, with how long that makes it, for the end.
    fn time_list(&self, handle: &ScrollHandle, r: &EditRef, start_list: bool, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let th = self.theme;
        let day = r.start.with_timezone(&Local).date_naive();
        let at = |t: chrono::NaiveDateTime| Local.from_local_datetime(&t).earliest().map(|t| t.with_timezone(&Utc));
        let length = r.end - r.start;
        let items: Vec<(String, chrono::DateTime<Utc>, chrono::DateTime<Utc>, bool)> = if start_list {
            let mut times: Vec<chrono::DateTime<Utc>> = (0..96).filter_map(|k| at(day.and_hms_opt(0, 0, 0)? + chrono::Duration::minutes(15 * k))).collect();
            // The event's own start, if it's off the quarter hours (11:10).
            if !times.contains(&r.start) {
                times.push(r.start);
                times.sort();
            }
            times.into_iter().map(|t| (t.with_timezone(&Local).format("%H:%M").to_string(), t, t + length, t == r.start)).collect()
        } else {
            // Quarter hours after the start, plus the event's own end if it's between them.
            let mut ends: Vec<i64> = (1..=96).map(|k| 15 * k).collect();
            let own = (r.end - r.start).num_minutes();
            if own > 0 && !ends.contains(&own) {
                ends.push(own);
                ends.sort();
            }
            ends.into_iter()
                .map(|mins| {
                    let end = r.start + chrono::Duration::minutes(mins);
                    let dur = match (mins / 60, mins % 60) {
                        (0, m) => format!("{m}m"),
                        (h, 0) => format!("{h}h"),
                        (h, m) => format!("{h}h {m}m"),
                    };
                    let next_day = if end.with_timezone(&Local).date_naive() != day { " +1" } else { "" };
                    (format!("{}{next_day}  ({dur})", end.with_timezone(&Local).format("%H:%M")), r.start, end, end == r.end)
                })
                .collect()
        };
        div()
            .id(if start_list { "start-list" } else { "end-list" })
            .w(px(if start_list { 92. } else { 150. }))
            .h(px(TIME_ROW * 8.))
            .overflow_y_scroll()
            .track_scroll(handle)
            .children(items.into_iter().enumerate().map(|(k, (label, ns, ne, current))| {
                div()
                    .id(("time", k))
                    .h(px(TIME_ROW))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .text_size(px(13.))
                    .text_color(if current { th.ink } else { th.muted })
                    .when(current, |d| d.bg(with_alpha(th.ink, 0.08)).font_weight(FontWeight::SEMIBOLD))
                    .cursor_pointer()
                    .hover(move |d| d.bg(with_alpha(th.ink, 0.06)).text_color(th.ink))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_times(ns, ne, cx)))
            }))
    }

    /// A month to pick a day from; moving the event keeps its times (and an all-day event its length).
    fn month_grid(&self, month: NaiveDate, r: &EditRef, cx: &mut Context<Self>) -> Div {
        let th = self.theme;
        let first = month.with_day(1).unwrap_or(month);
        let current = day_of(r);
        let today = Local::now().date_naive();
        // Weeks start on Monday, as calm's do.
        let lead = first.weekday().num_days_from_monday() as i64;
        let start = first - chrono::Duration::days(lead);
        let shift_month = |m: NaiveDate, by: i32| {
            let total = m.year() * 12 + m.month0() as i32 + by;
            NaiveDate::from_ymd_opt(total.div_euclid(12), total.rem_euclid(12) as u32 + 1, 1).unwrap_or(m)
        };
        let (prev, next) = (shift_month(first, -1), shift_month(first, 1));
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .pb(px(4.))
            .child(self.button("month-prev", "‹").on_click(cx.listener(move |this, _, _, cx| {
                this.picker = Some(Picker::Date(prev));
                cx.notify();
            })))
            .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(first.format("%B %Y").to_string()))
            .child(self.button("month-next", "›").on_click(cx.listener(move |this, _, _, cx| {
                this.picker = Some(Picker::Date(next));
                cx.notify();
            })));
        let weekdays = div().flex().children(["M", "T", "W", "T", "F", "S", "S"].map(|d| div().w(px(32.)).text_size(px(11.)).text_color(th.faint).flex().justify_center().child(d)));
        let (rs, re) = (r.start, r.end);
        let weeks = (0..6).map(|w| {
            div().flex().children((0..7).map(|k| {
                let day = start + chrono::Duration::days(w * 7 + k);
                let in_month = day.month() == first.month();
                let chosen = day == current;
                div()
                    .id(("day", (w * 7 + k) as usize))
                    .w(px(32.))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .text_size(px(12.5))
                    .text_color(if chosen { th.bg } else if day == today { th.now } else if in_month { th.ink } else { th.faint })
                    .when(chosen, |d| d.bg(th.ink).font_weight(FontWeight::SEMIBOLD))
                    .cursor_pointer()
                    .hover(move |d| if chosen { d } else { d.bg(with_alpha(th.ink, 0.08)) })
                    .child(day.day().to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        // Same wall-clock times on the new day.
                        let delta = chrono::Duration::days((day - current).num_days());
                        this.set_times(rs + delta, re + delta, cx);
                    }))
            }))
        });
        div().flex().flex_col().child(header).child(weekdays).children(weeks)
    }
}

/// The event's (first) day: all-day dates are stored at UTC midnight, timed events are local.
fn day_of(r: &EditRef) -> NaiveDate {
    if r.all_day { r.start.date_naive() } else { r.start.with_timezone(&Local).date_naive() }
}
