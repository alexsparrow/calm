//! Running commands: what keys (and the `:` prompt) do. The keymap is config
//! (`assets/keys.kdl`, then yours, then the view's own); this is the other half.

use gpui::{Context, Keystroke};

use super::CalmView;
use super::search::Kind;
use crate::keys::{Command, Ctx, Press};
use crate::model;
use crate::spec::{Bind, CHANNELS};

pub(super) fn press(k: &Keystroke) -> Press {
    Press {
        key: k.key.clone(),
        typed: k.key_char.clone(),
        ctrl: k.modifiers.control,
        alt: k.modifiers.alt,
        cmd: k.modifiers.platform,
        shift: k.modifiers.shift,
    }
}

impl CalmView {
    /// Which keys apply: a mode's, or the normal ones. (The search field's are looked up by
    /// the field.)
    fn key_ctx(&self) -> Ctx {
        if self.creating() {
            Ctx::Create
        } else if self.adjusting() {
            Ctx::Adjust
        } else {
            Ctx::Normal
        }
    }

    /// The command for a key: the mode's first, then the view's own (while what they change
    /// is offered), then the normal keys.
    pub(super) fn command_for(&self, ctx: Ctx, p: &Press) -> Option<Command> {
        let keys = &self.conf.keys;
        if ctx != Ctx::Normal
            && let Some(c) = keys.lookup(ctx, p)
        {
            return Some(c.clone());
        }
        if ctx == Ctx::Search {
            return None;
        }
        let mine = self.spec().keys.iter().find(|b| b.chord.matches(p) && self.offered(&b.command));
        mine.map(|b| b.command.clone()).or_else(|| keys.lookup(Ctx::Normal, p).cloned())
    }

    /// Whether the view offers what a command changes, at this zoom: its params (while
    /// they apply) or session channels.
    pub(super) fn offered(&self, cmd: &Command) -> bool {
        let (spec, values, mark) = (self.spec(), &self.values[self.preset], self.lens().mark);
        cmd.params().iter().all(|n| match spec.param(n) {
            Some(p) => spec.active(p, values, mark),
            None => CHANNELS.iter().any(|(c, _)| c == n),
        })
    }

    pub(super) fn on_key(&mut self, ev: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        let ctx = self.key_ctx();
        if let Some(cmd) = self.command_for(ctx, &press(&ev.keystroke))
            && self.run(&cmd, cx)
        {
            cx.notify();
        }
    }

    /// Does a command. False if it didn't apply (so the key can go elsewhere).
    pub(super) fn run(&mut self, cmd: &Command, cx: &mut Context<Self>) -> bool {
        if self.mode_command(cmd, cx) {
            return true;
        }
        match cmd {
            Command::Pan(n) => self.pan(*n),
            Command::Scroll(n) if self.scrolls_hours() => self.scroll_hours(*n as f32 / 24., 1., 0.5),
            Command::Scroll(n) => self.pan(*n),
            Command::Zoom(n) => self.zoom_step(*n),
            Command::Today => self.go_today(),
            Command::FullDay => self.toggle_full_day(),
            Command::View(i) => {
                if *i >= self.views.len() {
                    return false;
                }
                self.set_preset(*i);
            }
            Command::ViewStep(d) => {
                let n = self.views.len() as i32;
                self.set_preset((self.preset as i32 + d).rem_euclid(n) as usize);
            }
            Command::Search => self.open_prompt(Kind::Search),
            Command::Prompt => self.open_prompt(Kind::Jump),
            Command::Match(_) if self.prompt.is_some() => return self.prompt_command(cmd, cx),
            Command::Match(d) => return self.next_match(*d),
            Command::Complete => return self.prompt_command(cmd, cx),
            Command::Create => self.toggle_mode(true),
            Command::Adjust => self.toggle_mode(false),
            Command::Delete if self.selected.is_some() => self.delete_selected(cx),
            Command::Delete => return false,
            Command::Changes => self.changes_on = !self.changes_on,
            Command::Sync => self.sync_now(),
            Command::Editor => {
                self.editor_on = !self.editor_on;
                self.editor_note = None;
            }
            Command::Menu => self.menu_on = !self.menu_on,
            Command::Theme => self.toggle_theme(),
            Command::Help => self.help_on = !self.help_on,
            Command::Stats => self.hud_on = !self.hud_on,
            Command::Close => self.close_one(),
            Command::Stress => {
                self.stress = !self.stress;
                self.set_data(model::generate(if self.stress { 3 } else { 1 }));
            }
            // Mode commands outside a mode.
            Command::Step(_) | Command::Stretch(_) | Command::Place | Command::Done | Command::Back | Command::Snap(_) | Command::FreeOnly => return false,
            Command::Cycle(name) => return self.cycle_param(name, cx),
            Command::Set(a) => return self.set_params(a, cx),
            Command::Toggle(a) => return self.toggle(a, cx),
            Command::Overlay(name) => {
                let Some(o) = self.conf.overlays.iter().find(|o| o.name == *name) else {
                    self.toast(format!("No overlay called \u{201c}{name}\u{201d}"), None, cx);
                    return false;
                };
                let set = o.set.clone();
                return self.toggle(&set, cx);
            }
        }
        true
    }

    /// Esc: the innermost open thing closes, else the search, else the selection.
    fn close_one(&mut self) {
        if self.picker.is_some() {
            self.picker = None;
        } else if self.menu_on || self.params_open || self.changes_on {
            self.menu_on = false;
            self.params_open = false;
            self.changes_on = false;
        } else if self.help_on {
            self.help_on = false;
        } else if self.editor_on {
            self.editor_on = false;
        } else if self.found.is_some() {
            self.clear_search(false);
        } else {
            self.selected = None;
        }
    }

    /// A param: toggles flip, segmented ones step through the options that apply now.
    fn cycle_param(&mut self, name: &str, cx: &mut Context<Self>) -> bool {
        let (spec, values, mark) = (self.spec(), &self.values[self.preset], self.lens().mark);
        let Some(p) = spec.param(name) else {
            self.toast(format!("The {} view has no \u{201c}{name}\u{201d}", spec.name), None, cx);
            return false;
        };
        if !spec.active(p, values, mark) {
            return false;
        }
        let current = spec.value(name, values, mark);
        let next = match p.bind {
            Bind::Toggle => (if current == "true" { "false" } else { "true" }).to_string(),
            Bind::Segmented => {
                let opts: Vec<&str> = spec.options(p, values, mark).map(|o| o.value.as_str()).collect();
                let i = opts.iter().position(|v| *v == current).map_or(0, |i| (i + 1) % opts.len().max(1));
                opts.get(i).map(|v| v.to_string()).unwrap_or(current)
            }
        };
        self.set_param(name, &next);
        true
    }

    /// A setting's value here: a param of this view, or a session channel (`rail`, `zones`).
    fn setting(&self, name: &str) -> Option<String> {
        let (spec, values, mark) = (self.spec(), &self.values[self.preset], self.lens().mark);
        if spec.param(name).is_some() {
            return Some(spec.value(name, values, mark));
        }
        CHANNELS.iter().any(|(c, _)| *c == name).then(|| self.over.get(name).cloned().unwrap_or_else(|| spec.own_channel(name, values, mark)))
    }

    /// `toggle a=b …`: set them, or, when they're all set already, put back what they were
    /// (session channels go back to the view's own).
    fn toggle(&mut self, a: &[(String, String)], cx: &mut Context<Self>) -> bool {
        let on = a.iter().all(|(k, v)| self.setting(k).as_deref() == Some(v.as_str()));
        let params = a.iter().any(|(k, _)| self.spec().param(k).is_some());
        // Channels are the session's, so their toggles are too; params belong to a view.
        let key = (if params { self.spec().name.clone() } else { String::new() }, Command::Toggle(a.to_vec()).to_string());
        if on {
            let back = self.restore.remove(&key).unwrap_or_else(|| a.iter().map(|(k, _)| (k.clone(), None)).collect());
            for (k, v) in back {
                match v {
                    Some(v) => self.put(&k, &v),
                    None => self.reset(&k),
                }
            }
            return true;
        }
        let before: Vec<(String, Option<String>)> =
            a.iter().map(|(k, _)| (k.clone(), if self.spec().param(k).is_some() { self.setting(k) } else { self.over.get(k).cloned() })).collect();
        if !self.set_params(a, cx) {
            return false;
        }
        self.restore.insert(key, before);
        true
    }

    /// Sets a param of this view, or a session channel.
    fn put(&mut self, name: &str, value: &str) {
        if self.spec().param(name).is_some() {
            self.set_param(name, value);
        } else {
            self.over.insert(name.to_string(), value.to_string());
            self.dirty = true;
        }
    }

    /// Back to the default: a param's, or the view's own for a channel.
    fn reset(&mut self, name: &str) {
        match self.spec().param(name).map(|p| p.default.clone()) {
            Some(d) => self.set_param(name, &d),
            None => {
                self.over.remove(name);
                self.dirty = true;
            }
        }
    }

    /// `set a=b c=d`: all of them, or none if any doesn't fit this view (with a note why).
    fn set_params(&mut self, a: &[(String, String)], cx: &mut Context<Self>) -> bool {
        let spec = self.spec();
        let problem = a.iter().find_map(|(k, v)| {
            let Some(p) = spec.param(k) else {
                return match CHANNELS.iter().find(|(c, _)| c == k) {
                    Some((_, values)) if !values.contains(&v.as_str()) => Some(format!("\u{201c}{v}\u{201d} isn't a {k}: {}", values.join(", "))),
                    Some(_) => None,
                    None => Some(format!("The {} view has no \u{201c}{k}\u{201d}", spec.name)),
                };
            };
            let ok = match p.bind {
                Bind::Toggle => v == "true" || v == "false",
                Bind::Segmented => p.options.iter().any(|o| o.value == *v),
            };
            let choices = || match p.bind {
                Bind::Toggle => "true, false".to_string(),
                Bind::Segmented => p.options.iter().map(|o| o.value.as_str()).collect::<Vec<_>>().join(", "),
            };
            (!ok).then(|| format!("\u{201c}{v}\u{201d} isn't a {k}: {}", choices()))
        });
        if let Some(why) = problem {
            self.toast(why, None, cx);
            return false;
        }
        for (k, v) in a {
            self.put(k, v);
        }
        true
    }
}
