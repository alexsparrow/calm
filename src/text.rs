//! Text measurement and fitting on top of gpui's shaper. gpui caches line
//! layouts across frames, so measuring every visible label each frame is cheap.

use gpui::{Font, FontWeight, Hsla, Pixels, ShapedLine, SharedString, TextRun, Window, font, px};

#[derive(Clone)]
pub struct Fonts {
    pub regular: Font,
    pub medium: Font,
    pub semibold: Font,
}

impl Fonts {
    pub fn new() -> Self {
        let with = |weight| Font { weight, ..font("Inter") };
        Self { regular: with(FontWeight::NORMAL), medium: with(FontWeight::MEDIUM), semibold: with(FontWeight::SEMIBOLD) }
    }
}

pub fn shape(window: &Window, text: SharedString, font: &Font, size: f32, color: Hsla) -> ShapedLine {
    let run = TextRun {
        len: text.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window.text_system().shape_line(text, px(size), &[run], None)
}

pub fn width(window: &Window, text: &SharedString, font: &Font, size: f32) -> f32 {
    f32::from(shape(window, text.clone(), font, size, gpui::black()).width)
}

type FitCache = std::collections::HashMap<(SharedString, u32, u32, i32), Option<(SharedString, f32)>>;

thread_local! {
    /// Fitted labels by (text, weight, size, width bucket). Zooming and morphs ask the
    /// same questions every frame; shaping and ellipsising each time is the cost.
    static FIT: std::cell::RefCell<FitCache> =
        Default::default();
}

/// Returns `text`, or a prefix ending in "…", that fits in `max` pixels.
/// `None` when not even a few characters would fit: better no label than "D…".
pub fn fit(window: &Window, text: &SharedString, font: &Font, size: f32, max: f32) -> Option<(SharedString, f32)> {
    // 3px buckets, rounded down so a cached fit never overflows.
    let bucket = (max / 3.).floor() as i32;
    let key = (text.clone(), font.weight.0 as u32, size.to_bits(), bucket);
    if let Some(hit) = FIT.with(|c| c.borrow().get(&key).cloned()) {
        return hit;
    }
    let out = fit_uncached(window, text, font, size, bucket as f32 * 3.);
    FIT.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 50_000 {
            c.clear();
        }
        c.insert(key, out.clone());
    });
    out
}

fn fit_uncached(window: &Window, text: &SharedString, font: &Font, size: f32, max: f32) -> Option<(SharedString, f32)> {
    let line = shape(window, text.clone(), font, size, gpui::black());
    let full = f32::from(line.width);
    if full <= max {
        return Some((text.clone(), full));
    }
    let ellipsis = width(window, &"…".into(), font, size);
    let budget = max - ellipsis;
    let glyphs = line.runs.iter().flat_map(|r| r.glyphs.iter());
    let cut = glyphs
        .take_while(|g| f32::from(g.position.x) <= budget)
        .last()
        .map(|g| g.index)?;
    // `cut` is the byte index of the first glyph that no longer fits whole;
    // the glyph before it ends at `g.position.x`, so take text before `cut`.
    let prefix = text[..cut].trim_end();
    if prefix.chars().count() < 3 {
        return None;
    }
    let s: SharedString = format!("{prefix}…").into();
    let w = width(window, &s, font, size);
    Some((s, w.min(max)))
}

pub fn line_height(size: f32) -> Pixels {
    px((size * 1.3).round())
}
