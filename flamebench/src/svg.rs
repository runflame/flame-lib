//! The charts, as hand-written SVG.
//!
//! A port of the approved prototypes (`prompts/svg-prototypes/generate.py`
//! and `bench2.py`): their color tokens, the geometry of each chart type,
//! and their helpers for text, bars, legends and the SVG wrapper. Every
//! chart is drawn once and written twice, `<name>.light.svg` and
//! `<name>.dark.svg`, differing only in `data-theme`: GitHub picks one per
//! reader theme with `<picture>`, because an SVG's own
//! `prefers-color-scheme` follows the operating system inside an `<img>`,
//! not GitHub's theme. The media block stays, so a copy without
//! `data-theme` still follows its viewer.
//!
//! [`charts`] draws report 1's seven charts from a results file;
//! [`scaling_charts`] draws verifier scaling's two, for report 2.
//!
//! Rules the design fixes:
//!
//! - colors come from the tokens only; the series slots are blue, orange
//!   and aqua, plus yellow as a fourth only in stacked bars, where just
//!   neighbors touch (validated for adjacent pairs, not all pairs); the one
//!   status color is critical
//!   red, and it never carries a status alone: an icon and a label ride
//!   with it;
//! - bars are at most 24 px thick with a 4 px rounded end and a square
//!   base, stacked segments are parted by a 2 px surface gap, lines are
//!   2 px, dots have radius 5 and a 2 px surface ring, gridlines are solid
//!   1 px;
//! - text uses the ink tokens, never a series color;
//! - a legend whenever there are two or more series; direct labels only on
//!   the points the story is about;
//! - `role="img"` with a `<title>` and `<desc>`, and a `<title>` on every
//!   mark, so hovering shows the exact value.
//!
//! Text cannot be measured here, so its width is estimated from the
//! character count, as the prototype does, and margins are generous.

use std::fmt::Write as _;

use crate::cpu::Region;
use crate::results::{CapacityAttempt, Results, ShapeResult};
use crate::scaling::Scaling;

/// Every chart's width.
pub const WIDTH: f64 = 720.0;

const FONT: &str = r#"system-ui, -apple-system, "Segoe UI", sans-serif"#;

/// The light theme's tokens.
const LIGHT: [(&str, &str); 11] = [
    ("surface", "#fcfcfb"),
    ("ink", "#0b0b0b"),
    ("ink2", "#52514e"),
    ("muted", "#898781"),
    ("grid", "#e1e0d9"),
    ("axis", "#c3c2b7"),
    ("band", "#f0efec"),
    ("s1", "#2a78d6"),
    ("s2", "#eb6834"),
    ("s3", "#1baf7a"),
    ("s4", "#eda100"),
];

/// The dark theme's tokens.
const DARK: [(&str, &str); 11] = [
    ("surface", "#1a1a19"),
    ("ink", "#ffffff"),
    ("ink2", "#c3c2b7"),
    ("muted", "#898781"),
    ("grid", "#2c2c2a"),
    ("axis", "#383835"),
    ("band", "#262624"),
    ("s1", "#3987e5"),
    ("s2", "#d95926"),
    ("s3", "#199e70"),
    ("s4", "#c98500"),
];

/// Left edge of titles, legends and notes.
const PAD: f64 = 24.0;
/// Baseline step between lines of 11–12 px text.
const LINE: f64 = 14.0;
/// Bar thickness: at most 24 px.
const BAR: f64 = 20.0;
/// Row pitch of the horizontal bar charts.
const ROW: f64 = 40.0;
/// How far above and below a row's middle the gas check's two prediction
/// lanes run.
const LANE: f64 = 6.0;

/// Which file a chart is rendered into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
}

impl Theme {
    pub const ALL: [Theme; 2] = [Theme::Light, Theme::Dark];

    /// `light` or `dark`: the `data-theme` value and the file suffix.
    pub fn name(self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }
}

/// One drawn chart.
pub struct Chart {
    /// The file stem: `<name>.light.svg`, `<name>.dark.svg`.
    pub name: &'static str,
    /// The `<img>` alt text in the docs page.
    pub alt: String,
    title: String,
    desc: String,
    height: f64,
    body: String,
}

impl Chart {
    /// The file name for `theme`.
    pub fn file_name(&self, theme: Theme) -> String {
        format!("{}.{}.svg", self.name, theme.name())
    }

    /// The whole SVG document for `theme`.
    pub fn render(&self, theme: Theme) -> String {
        let height = self.height;
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" data-theme=\"{theme}\" width=\"{WIDTH}\" \
             height=\"{height:.0}\" viewBox=\"0 0 {WIDTH} {height:.0}\" role=\"img\" \
             aria-labelledby=\"t d\"><title id=\"t\">{title}</title><desc id=\"d\">{desc}</desc>\
             {style}<rect class=\"bg\" width=\"{WIDTH}\" height=\"{height:.0}\" rx=\"8\"/>{body}</svg>\n",
            theme = theme.name(),
            title = esc(&self.title),
            desc = esc(&self.desc),
            style = style(),
            body = self.body,
        )
    }
}

/// Draws report 1's seven charts from a results file.
pub fn charts(results: &Results) -> Vec<Chart> {
    vec![
        cost_by_shape(results),
        tps_per_core(results),
        send_latency(results),
        send_breakdown(results),
        proof_capacity(results),
        peak_memory(results),
        gas_check(results),
    ]
}

/// Draws verifier scaling's two charts.
pub fn scaling_charts(scaling: &Scaling) -> Vec<Chart> {
    vec![verifier_scaling(scaling), verifier_efficiency(scaling)]
}

/// The status color critical: a shape that does not prove.
const CRITICAL: &str = "#d03b3b";

fn vars(tokens: &[(&str, &str)]) -> String {
    tokens
        .iter()
        .map(|(name, value)| format!("--{name}:{value};"))
        .collect()
}

fn style() -> String {
    format!(
        r#"<style>
svg {{ {light} font-family: {FONT}; }}
@media (prefers-color-scheme: dark) {{ svg:not([data-theme="light"]) {{ {dark} }} }}
svg[data-theme="dark"] {{ {dark} }}
.bg {{ fill: var(--surface); }}
.title {{ fill: var(--ink); font-size: 16px; font-weight: 600; }}
.sub {{ fill: var(--ink2); font-size: 12px; }}
.label {{ fill: var(--ink2); font-size: 12px; }}
.value {{ fill: var(--ink); font-size: 12px; font-weight: 600; font-variant-numeric: tabular-nums; }}
.tick {{ fill: var(--muted); font-size: 11px; font-variant-numeric: tabular-nums; }}
.note {{ fill: var(--muted); font-size: 11px; }}
.grid {{ stroke: var(--grid); stroke-width: 1; }}
.axis {{ stroke: var(--axis); stroke-width: 1; }}
.s1 {{ fill: var(--s1); }} .s2 {{ fill: var(--s2); }} .s3 {{ fill: var(--s3); }} .s4 {{ fill: var(--s4); }}
.l1 {{ stroke: var(--s1); stroke-width: 2; fill: none; stroke-linejoin: round; stroke-linecap: round; }}
.ideal {{ stroke: var(--muted); stroke-width: 1.5; fill: none; }}
.ref {{ stroke: var(--ink2); stroke-width: 1; }}
.dot1 {{ fill: var(--s1); stroke: var(--surface); stroke-width: 2; }}
.dot2 {{ fill: var(--s2); stroke: var(--surface); stroke-width: 2; }}
.dot3 {{ fill: var(--s3); stroke: var(--surface); stroke-width: 2; }}
.crit {{ fill: {CRITICAL}; }}
.crit-ink {{ fill: var(--ink); font-size: 12px; font-weight: 600; }}
.link {{ stroke: var(--axis); stroke-width: 2; }}
.mark {{ stroke: var(--surface); stroke-width: 1; }}
.band {{ fill: var(--band); }}
.band.alt {{ fill-opacity: 0.5; }}
</style>"#,
        light = vars(&LIGHT),
        dark = vars(&DARK),
    )
}

/// Escapes text for XML content and attributes.
pub fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
    out
}

/// Estimated width of `text` in the given class: 0.6 em per character,
/// a little more for the bold values.
pub fn text_width(text: &str, class: &str) -> f64 {
    let (size, em) = match class {
        "title" => (16.0, 0.62),
        "value" => (12.0, 0.62),
        "sub" | "label" => (12.0, 0.6),
        _ => (11.0, 0.6),
    };
    text.chars().count() as f64 * size * em
}

/// Splits `text` into lines no wider than `width`.
fn wrap(text: &str, class: &str, width: f64) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if text_width(&format!("{line} {word}"), class) <= width => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_owned()),
        }
    }
    lines
}

#[derive(Clone, Copy)]
enum Anchor {
    Start,
    Middle,
    End,
}

impl Anchor {
    fn name(self) -> &'static str {
        match self {
            Anchor::Start => "start",
            Anchor::Middle => "middle",
            Anchor::End => "end",
        }
    }
}

/// An SVG body under construction.
#[derive(Default)]
struct Body(String);

impl Body {
    fn text(&mut self, x: f64, y: f64, text: &str, class: &str, anchor: Anchor) {
        let _ = write!(
            self.0,
            r#"<text x="{x:.1}" y="{y:.1}" class="{class}" text-anchor="{}">{}</text>"#,
            anchor.name(),
            esc(text)
        );
    }

    fn line(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, class: &str) {
        let _ = write!(
            self.0,
            r#"<line x1="{x1:.1}" y1="{y1:.1}" x2="{x2:.1}" y2="{y2:.1}" class="{class}"/>"#
        );
    }

    fn line_titled(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, class: &str, tip: &str) {
        let _ = write!(
            self.0,
            r#"<line x1="{x1:.1}" y1="{y1:.1}" x2="{x2:.1}" y2="{y2:.1}" class="{class}"><title>{}</title></line>"#,
            esc(tip)
        );
    }

    fn polyline(&mut self, points: &[(f64, f64)], class: &str, tip: &str) {
        let points: Vec<String> = points
            .iter()
            .map(|(x, y)| format!("{x:.1},{y:.1}"))
            .collect();
        let _ = write!(
            self.0,
            r#"<polyline points="{}" class="{class}"><title>{}</title></polyline>"#,
            points.join(" "),
            esc(tip)
        );
    }

    fn dot(&mut self, x: f64, y: f64, class: &str, tip: &str) {
        let _ = write!(
            self.0,
            r#"<circle cx="{x:.1}" cy="{y:.1}" r="5" class="dot {class}"><title>{}</title></circle>"#,
            esc(tip)
        );
    }

    /// A vertical mark across a row at `x`, centered on `y`: the value the
    /// other marks of the row are read against.
    fn mark(&mut self, x: f64, y: f64, class: &str, tip: &str) {
        let _ = write!(
            self.0,
            r#"<rect x="{:.1}" y="{:.1}" width="4" height="{BAR}" rx="2" class="mark {class}"><title>{}</title></rect>"#,
            x - 2.0,
            y - BAR / 2.0,
            esc(tip)
        );
    }

    fn band(&mut self, x: f64, y: f64, width: f64, height: f64, alt: bool, tip: &str) {
        let class = if alt { "band alt" } else { "band" };
        let _ = write!(
            self.0,
            r#"<rect x="{x:.1}" y="{y:.1}" width="{width:.1}" height="{height:.1}" class="{class}"><title>{}</title></rect>"#,
            esc(tip)
        );
    }

    /// A horizontal bar segment from `x0` to `x1`: a square start, and a
    /// 4 px rounded end when `round_end`.
    fn hbar(&mut self, x0: f64, y: f64, x1: f64, class: &str, round_end: bool, tip: &str) {
        let width = (x1 - x0).max(1.5);
        let radius = if round_end {
            4f64.min(width / 2.0).min(BAR / 2.0)
        } else {
            0.0
        };
        let tip = esc(tip);
        if radius == 0.0 {
            let _ = write!(
                self.0,
                r#"<rect x="{x0:.1}" y="{y:.1}" width="{width:.1}" height="{BAR}" class="bar {class}"><title>{tip}</title></rect>"#
            );
            return;
        }
        let x1 = x0 + width;
        let _ = write!(
            self.0,
            r#"<path d="M{x0:.1},{y:.1} H{:.1} A{radius:.1},{radius:.1} 0 0 1 {x1:.1},{:.1} V{:.1} A{radius:.1},{radius:.1} 0 0 1 {:.1},{:.1} H{x0:.1} Z" class="bar {class}"><title>{tip}</title></path>"#,
            x1 - radius,
            y + radius,
            y + BAR - radius,
            x1 - radius,
            y + BAR,
        );
    }

    /// A legend from `(x, y)`, wrapping at the chart's right padding.
    /// Returns the baseline of its last row.
    fn legend(&mut self, x: f64, y: f64, items: &[Key]) -> f64 {
        let (mut cx, mut cy) = (x, y);
        for key in items {
            let key_width = if key.line { 18.0 } else { 10.0 };
            let width = key_width + 6.0 + text_width(key.label, "label") + 22.0;
            if cx > x && cx + width - 22.0 > WIDTH - PAD {
                cx = x;
                cy += 20.0;
            }
            if key.line {
                self.line(cx, cy - 4.0, cx + 18.0, cy - 4.0, key.class);
            } else {
                let _ = write!(
                    self.0,
                    r#"<rect x="{cx:.1}" y="{:.1}" width="10" height="10" rx="2" class="key {}"/>"#,
                    cy - 9.0,
                    key.class
                );
            }
            self.text(cx + key_width + 6.0, cy, key.label, "label", Anchor::Start);
            cx += width;
        }
        cy
    }

    /// Notes under the plot, wrapped. Returns the height they take.
    fn notes(&mut self, top: f64, notes: &[String]) -> f64 {
        let mut y = top;
        for note in notes {
            for line in wrap(note, "note", WIDTH - 2.0 * PAD) {
                y += LINE;
                self.text(PAD, y, &line, "note", Anchor::Start);
            }
        }
        y - top
    }
}

/// A legend key: a square for bars and dots, a line for line series.
struct Key {
    class: &'static str,
    label: &'static str,
    line: bool,
}

const fn square(class: &'static str, label: &'static str) -> Key {
    Key {
        class,
        label,
        line: false,
    }
}

const fn line_key(class: &'static str, label: &'static str) -> Key {
    Key {
        class,
        label,
        line: true,
    }
}

/// A linear axis from zero: its top and its step.
struct Axis {
    max: f64,
    step: f64,
}

impl Axis {
    /// About `ticks` steps of 1, 2, 2.5 or 5 times a power of ten, the top
    /// at or above `max`.
    fn nice(max: f64, ticks: f64) -> Axis {
        if !(max.is_finite() && max > 0.0) {
            return Axis {
                max: 1.0,
                step: 0.25,
            };
        }
        let raw = max / ticks;
        let magnitude = 10f64.powf(raw.log10().floor());
        let step = [1.0, 2.0, 2.5, 5.0, 10.0]
            .iter()
            .map(|m| m * magnitude)
            .find(|step| *step >= raw)
            .unwrap_or(10.0 * magnitude);
        let max = (max / step - 1e-9).ceil().max(1.0) * step;
        Axis { max, step }
    }

    fn ticks(&self) -> Vec<f64> {
        let count = (self.max / self.step).round() as usize;
        (0..=count).map(|i| i as f64 * self.step).collect()
    }

    /// A tick label with as many decimals as the step needs.
    fn label(&self, value: f64) -> String {
        let mut decimals = 0;
        while decimals < 3 && (self.step * 10f64.powi(decimals)).fract().abs() > 1e-6 {
            decimals += 1;
        }
        if decimals == 0 {
            thousands(value.round() as u64)
        } else {
            format!("{value:.*}", decimals as usize)
        }
    }
}

/// An integer with comma thousands separators.
pub fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A rate, rounded to a whole number, with separators.
pub fn whole(value: f64) -> String {
    thousands(value.max(0.0).round() as u64)
}

/// The time unit a set of nanosecond values reads best in.
#[derive(Clone, Copy)]
pub struct TimeUnit {
    pub name: &'static str,
    pub ns: f64,
}

impl TimeUnit {
    /// Milliseconds when the largest value reaches one, else microseconds.
    pub fn for_max(max_ns: f64) -> TimeUnit {
        if max_ns >= 1e6 {
            TimeUnit {
                name: "ms",
                ns: 1e6,
            }
        } else {
            TimeUnit {
                name: "µs",
                ns: 1e3,
            }
        }
    }

    /// `ns` in this unit, to three significant digits.
    pub fn format(self, ns: f64) -> String {
        let value = ns / self.ns;
        if value < 10.0 {
            format!("{value:.2}")
        } else if value < 100.0 {
            format!("{value:.1}")
        } else {
            format!("{value:.0}")
        }
    }
}

/// Places labels at their wanted baselines, pushed apart until none is
/// closer than its predecessor's height, and kept within `lo..=hi`.
/// `labels` are `(wanted baseline, height)`; returns the baselines.
fn spread(labels: &[(f64, f64)], lo: f64, hi: f64) -> Vec<f64> {
    let mut order: Vec<usize> = (0..labels.len()).collect();
    order.sort_by(|a, b| labels[*a].0.total_cmp(&labels[*b].0));
    let mut placed = vec![0.0; labels.len()];
    let mut floor = lo;
    for &index in &order {
        let y = labels[index].0.max(floor);
        placed[index] = y;
        floor = y + labels[index].1;
    }
    // Pull back up from the bottom if the stack overran it.
    let mut ceiling = hi;
    for &index in order.iter().rev() {
        let limit = ceiling - (labels[index].1 - LINE).max(0.0);
        if placed[index] > limit {
            placed[index] = limit;
        }
        ceiling = placed[index] - LINE;
    }
    placed
}

/// The title and subtitle every chart starts with. Returns the baseline
/// under which the legend goes.
fn heading(body: &mut Body, title: &str, sub: &str) -> f64 {
    body.text(PAD, 32.0, title, "title", Anchor::Start);
    body.text(PAD, 52.0, sub, "sub", Anchor::Start);
    76.0
}

/// One row of a horizontal bar chart: a label, its segments in the chart's
/// unit, a tooltip per segment, and the text after the bar.
struct BarRow {
    label: String,
    values: Vec<f64>,
    tips: Vec<String>,
    value: String,
}

/// The plot of a horizontal bar chart, one row per [`BarRow`], stacked when
/// a row has several segments.
struct Bars<'a> {
    top: f64,
    left: f64,
    rows: &'a [BarRow],
    /// Each segment's class.
    classes: &'a [&'a str],
    /// The axis label under the plot.
    unit: &'a str,
    /// The text after each bar is a value, or a note.
    value_class: &'a str,
    /// The axis must reach at least this far.
    min_max: f64,
}

impl Bars<'_> {
    /// Draws the gridlines, the axis, the bars and their labels. Returns the
    /// baseline of the plot and the x scale's parameters: `(base_y, left,
    /// plot_w, axis_max)`.
    fn draw(&self, body: &mut Body) -> (f64, f64, f64, f64) {
        let right = self
            .rows
            .iter()
            .map(|row| text_width(&row.value, self.value_class))
            .fold(0.0, f64::max)
            + 40.0;
        let plot_w = WIDTH - self.left - right;
        let axis = Axis::nice(
            self.rows
                .iter()
                .map(|row| row.values.iter().sum::<f64>())
                .fold(self.min_max, f64::max),
            6.0,
        );
        let left = self.left;
        let sx = |v: f64| left + v / axis.max * plot_w;
        let top = self.top;
        let base_y = top + ROW * self.rows.len() as f64;

        for tick in axis.ticks() {
            let x = sx(tick);
            body.line(x, top - 6.0, x, base_y, "grid");
            body.text(x, base_y + 16.0, &axis.label(tick), "tick", Anchor::Middle);
        }
        body.text(left + plot_w, base_y + 32.0, self.unit, "note", Anchor::End);
        body.line(left, top - 6.0, left, base_y, "axis");

        for (index, row) in self.rows.iter().enumerate() {
            let y = top + index as f64 * ROW + (ROW - BAR) / 2.0;
            body.text(
                left - 12.0,
                y + BAR / 2.0 + 4.0,
                &row.label,
                "label",
                Anchor::End,
            );
            let mut x = left;
            let mut sum = 0.0;
            // Zero-width segments are left out, so the last drawn one gets
            // the rounded end.
            let drawn: Vec<usize> = (0..row.values.len())
                .filter(|i| row.values[*i] > 0.0)
                .collect();
            for (i, value) in row.values.iter().enumerate() {
                sum += value;
                let x1 = sx(sum);
                if !drawn.contains(&i) {
                    continue;
                }
                let last = drawn.last() == Some(&i);
                let end = if last { x1 } else { x1 - 2.0 };
                body.hbar(x, y, end, self.classes[i], last, &row.tips[i]);
                x = x1;
            }
            body.text(
                x + 8.0,
                y + BAR / 2.0 + 4.0,
                &row.value,
                self.value_class,
                Anchor::Start,
            );
        }
        (base_y, left, plot_w, axis.max)
    }
}

/// A chart's height from the baseline of its plot and its notes.
fn finish(body: &mut Body, base_y: f64, notes: &[String]) -> f64 {
    let notes_h = body.notes(base_y + 40.0, notes);
    (base_y + 40.0 + notes_h + 14.0).ceil()
}

fn cost_by_shape(results: &Results) -> Chart {
    let title = "Verify time by transaction shape, one core";
    let sub = "Inputs → outputs. Criterion medians; Utreexo checked once per input.";
    let unit = TimeUnit::for_max(
        results
            .shapes
            .iter()
            .map(ShapeResult::total_ns)
            .fold(0.0, f64::max),
    );
    // The first three parts run inside `verify`; Utreexo is the chain's
    // separate membership check, once per input.
    let parts = [
        square("s1", "Proof (estimated)"),
        square("s2", "VM and other"),
        square("s3", "Signature"),
        square("s4", "Utreexo, per input"),
    ];
    let rows: Vec<BarRow> = results
        .shapes
        .iter()
        .map(|shape| {
            let values = [
                shape.proof_ns(),
                shape.vm_and_other_ns(),
                shape.signature_ns,
                shape.utreexo_all_inputs_ns(),
            ];
            BarRow {
                label: shape.label.clone(),
                tips: values
                    .iter()
                    .zip(&parts)
                    .map(|(value, key)| {
                        format!(
                            "{}: {} {} {}",
                            shape.label,
                            key.label,
                            unit.format(*value),
                            unit.name
                        )
                    })
                    .collect(),
                values: values.iter().map(|ns| ns / unit.ns).collect(),
                value: format!("{} {}", unit.format(shape.total_ns()), unit.name),
            }
        })
        .collect();

    let small = |value: fn(&ShapeResult) -> f64| {
        let values: Vec<f64> = results.shapes.iter().map(value).collect();
        let low = values.iter().copied().fold(f64::INFINITY, f64::min);
        let high = values.iter().copied().fold(0.0, f64::max);
        format!("{}–{} µs", whole(low / 1_000.0), whole(high / 1_000.0))
    };
    let mut notes = vec![
        "Proof, VM and other, and signature run inside verify; Utreexo is the chain's separate \
         check, once per input."
            .to_owned(),
        format!(
            "Signature ({}) and Utreexo ({}) are too small to see at this scale; the table has \
             every value.",
            small(|shape| shape.signature_ns),
            small(ShapeResult::utreexo_all_inputs_ns),
        ),
        "Proof: estimated from a synthetic proof with the same multiplier count.".to_owned(),
        format!(
            "Utreexo: one membership check per input, in a forest of {} leaves.",
            results.shapes[0]
                .largest_forest()
                .map_or_else(|| "no".into(), |time| thousands(time.leaves))
        ),
    ];
    let flagged: Vec<&str> = results
        .shapes
        .iter()
        .filter(|shape| shape.estimate_overshot())
        .map(|shape| shape.label.as_str())
        .collect();
    if !flagged.is_empty() {
        notes.push(format!(
            "Flagged: the estimate exceeds verify minus signature for {}; VM and other is \
             clamped to zero.",
            and_list(&flagged)
        ));
    }

    let mut body = Body::default();
    let legend_y = heading(&mut body, title, sub);
    let legend_bottom = body.legend(PAD, legend_y, &parts);
    let (base_y, ..) = Bars {
        top: legend_bottom + 16.0,
        left: 84.0,
        rows: &rows,
        classes: &["s1", "s2", "s3", "s4"],
        unit: unit.name,
        value_class: "value",
        min_max: 0.0,
    }
    .draw(&mut body);
    let height = finish(&mut body, base_y, &notes);

    let desc = format!(
        "{sub} {}",
        results
            .shapes
            .iter()
            .map(|shape| format!(
                "{}: {} {u} in all, proof {} {u}, VM and other {} {u}, signature {} {u}, \
                 Utreexo {} {u}.",
                shape.label,
                unit.format(shape.total_ns()),
                unit.format(shape.proof_ns()),
                unit.format(shape.vm_and_other_ns()),
                unit.format(shape.signature_ns),
                unit.format(shape.utreexo_all_inputs_ns()),
                u = unit.name,
            ))
            .collect::<Vec<_>>()
            .join(" ")
    );
    Chart {
        name: "cost-by-shape",
        alt: "Verify time by transaction shape, one core".into(),
        title: title.into(),
        desc,
        height,
        body: body.0,
    }
}

fn tps_per_core(results: &Results) -> Chart {
    let title = "One core's verification rate";
    let sub = "Transactions one core verifies per second: verify plus one Utreexo check per input.";
    let rows: Vec<BarRow> = results
        .shapes
        .iter()
        .map(|shape| {
            let value = format!("~{}", whole(shape.tps_per_core()));
            BarRow {
                label: shape.label.clone(),
                values: vec![shape.tps_per_core()],
                tips: vec![format!("{}: {value} transactions per second", shape.label)],
                value,
            }
        })
        .collect();

    let mut body = Body::default();
    let _ = heading(&mut body, title, sub);
    let (base_y, ..) = Bars {
        top: 84.0,
        left: 84.0,
        rows: &rows,
        classes: &["s1"],
        unit: "transactions per second",
        value_class: "value",
        min_max: 0.0,
    }
    .draw(&mut body);
    let notes = ["One core, verification only: no block building or state updates.".to_owned()];
    let height = finish(&mut body, base_y, &notes);

    let desc = format!(
        "{sub} {}",
        rows.iter()
            .map(|row| format!("{}: {}.", row.label, row.value))
            .collect::<Vec<_>>()
            .join(" "),
    );
    Chart {
        name: "tps-per-core",
        alt: "One core's verification rate, by transaction shape".into(),
        title: title.into(),
        desc,
        height,
        body: body.0,
    }
}

/// `+12%` or `−28%`: how far a prediction lies from the measurement.
pub fn signed_percent(fraction: f64) -> String {
    let percent = (fraction * 100.0).round();
    if percent < 0.0 {
        format!("−{:.0}%", -percent)
    } else {
        format!("+{percent:.0}%")
    }
}

fn gas_check(results: &Results) -> Chart {
    let title = "Measured verify time against the gas schedule";
    let sub = "Predicted time is gas × 100 ns of verifier work.";
    let unit = TimeUnit::for_max(
        results
            .shapes
            .iter()
            .map(|shape| {
                shape
                    .verify_ns
                    .max(shape.gas_predicted_ns())
                    .max(shape.gas_per_gate_predicted_ns())
            })
            .fold(0.0, f64::max),
    );
    let labels: Vec<String> = results
        .shapes
        .iter()
        .map(|shape| {
            format!(
                "today {} · per gate {}",
                signed_percent(shape.gas_error(shape.gas_predicted_ns())),
                signed_percent(shape.gas_error(shape.gas_per_gate_predicted_ns())),
            )
        })
        .collect();

    let mut body = Body::default();
    let legend_y = heading(&mut body, title, sub);
    let legend_bottom = body.legend(
        PAD,
        legend_y,
        &[
            square("s1", "Measured"),
            square("s2", "Predicted by gas today"),
            square("s3", "Predicted if mix paid per gate"),
        ],
    );
    let left = 84.0;
    let right = labels
        .iter()
        .map(|label| text_width(label, "note"))
        .fold(0.0, f64::max)
        + 36.0;
    let top = legend_bottom + 16.0;
    let plot_w = WIDTH - left - right;
    let axis = Axis::nice(
        results
            .shapes
            .iter()
            .map(|shape| {
                shape
                    .verify_ns
                    .max(shape.gas_predicted_ns())
                    .max(shape.gas_per_gate_predicted_ns())
                    / unit.ns
            })
            .fold(0.0, f64::max),
        6.0,
    );
    let sx = |ns: f64| left + ns / unit.ns / axis.max * plot_w;
    let base_y = top + ROW * results.shapes.len() as f64;

    for tick in axis.ticks() {
        let x = left + tick / axis.max * plot_w;
        body.line(x, top - 6.0, x, base_y, "grid");
        body.text(x, base_y + 16.0, &axis.label(tick), "tick", Anchor::Middle);
    }
    body.text(left + plot_w, base_y + 32.0, unit.name, "note", Anchor::End);
    for (index, (shape, label)) in results.shapes.iter().zip(&labels).enumerate() {
        let y = top + index as f64 * ROW + ROW / 2.0;
        let measured = sx(shape.verify_ns);
        let today = sx(shape.gas_predicted_ns());
        let per_gate = sx(shape.gas_per_gate_predicted_ns());
        let hi = measured.max(today).max(per_gate);
        body.text(left - 12.0, y + 4.0, &shape.label, "label", Anchor::End);
        // Each prediction on a lane of its own, linked to the measurement,
        // so a prediction that lands on the measurement stays visible.
        let (today_y, per_gate_y) = (y - LANE, y + LANE);
        body.line(measured, today_y, today, today_y, "link");
        body.line(measured, per_gate_y, per_gate, per_gate_y, "link");
        body.mark(
            measured,
            y,
            "s1",
            &format!(
                "{}: measured {} {}",
                shape.label,
                unit.format(shape.verify_ns),
                unit.name
            ),
        );
        body.dot(
            today,
            today_y,
            "dot2",
            &format!(
                "{}: predicted by gas today {} {} ({} gas)",
                shape.label,
                unit.format(shape.gas_predicted_ns()),
                unit.name,
                thousands(shape.gas_used)
            ),
        );
        body.dot(
            per_gate,
            per_gate_y,
            "dot3",
            &format!(
                "{}: predicted if mix paid per gate {} {} ({} gas)",
                shape.label,
                unit.format(shape.gas_per_gate_predicted_ns()),
                unit.name,
                thousands(shape.gas_per_gate())
            ),
        );
        body.text(hi + 12.0, y + 4.0, label, "note", Anchor::Start);
    }
    let under = results
        .shapes
        .iter()
        .filter(|shape| shape.gas_error(shape.gas_predicted_ns()) < 0.0)
        .count();
    let notes = [
        if under == results.shapes.len() {
            "Gas today predicts less than was measured in every shape.".to_owned()
        } else if under == 0 {
            "Gas today predicts more than was measured in every shape.".to_owned()
        } else {
            format!(
                "Gas today predicts less than was measured in {under} of {} shapes.",
                results.shapes.len()
            )
        },
        "Per gate: mix charged 120 gas per real gate, not per (m + n)² item; every other \
         charge as today."
            .to_owned(),
    ];
    let height = finish(&mut body, base_y, &notes);

    let desc = format!(
        "{sub} {}",
        results
            .shapes
            .iter()
            .map(|shape| format!(
                "{}: measured {} {u}, predicted by gas today {} {u}, predicted if mix paid per \
                 gate {} {u}.",
                shape.label,
                unit.format(shape.verify_ns),
                unit.format(shape.gas_predicted_ns()),
                unit.format(shape.gas_per_gate_predicted_ns()),
                u = unit.name,
            ))
            .collect::<Vec<_>>()
            .join(" ")
    );
    Chart {
        name: "gas-check",
        alt: "Measured verify time against gas-predicted time today and with mix priced per gate, \
              by shape"
            .into(),
        title: title.into(),
        desc,
        height,
        body: body.0,
    }
}

fn send_latency(results: &Results) -> Chart {
    let title = "Time to send a payment, one core";
    let sub = "A send in a warm process, and what a new process adds on its first.";
    let unit = TimeUnit::for_max(
        results
            .shapes
            .iter()
            .map(|shape| shape.warm_send_ns() + shape.generator_setup_ns())
            .fold(0.0, f64::max),
    );
    let parts = [
        square("s1", "Warm send"),
        square("s2", "Generator setup, once per process"),
    ];
    let rows: Vec<BarRow> = results
        .shapes
        .iter()
        .map(|shape| {
            let values = [shape.warm_send_ns(), shape.generator_setup_ns()];
            BarRow {
                label: shape.label.clone(),
                tips: values
                    .iter()
                    .zip(&parts)
                    .map(|(value, key)| {
                        format!(
                            "{}: {} {} {}",
                            shape.label,
                            key.label,
                            unit.format(*value),
                            unit.name
                        )
                    })
                    .collect(),
                values: values.iter().map(|ns| ns / unit.ns).collect(),
                value: format!(
                    "{} warm + {} setup",
                    unit.format(shape.warm_send_ns()),
                    unit.format(shape.generator_setup_ns()),
                ),
            }
        })
        .collect();

    let mut body = Body::default();
    let legend_y = heading(&mut body, title, sub);
    let legend_bottom = body.legend(PAD, legend_y, &parts);
    let (base_y, ..) = Bars {
        top: legend_bottom + 16.0,
        left: 84.0,
        rows: &rows,
        classes: &["s1", "s2"],
        unit: unit.name,
        value_class: "value",
        min_max: 0.0,
    }
    .draw(&mut body);
    let notes = [
        "Warm: prepare, build, sign and package, criterion medians. Setup: the median first \
         send minus the median second send, in new processes."
            .to_owned(),
        "A wallet that runs one command per process pays the setup on every send.".to_owned(),
    ];
    let height = finish(&mut body, base_y, &notes);

    let desc = format!(
        "{sub} {}",
        results
            .shapes
            .iter()
            .map(|shape| format!(
                "{}: {} {u} warm, {} {u} of setup; a first send in a new process took {} {u}.",
                shape.label,
                unit.format(shape.warm_send_ns()),
                unit.format(shape.generator_setup_ns()),
                unit.format(shape.first_send_ns()),
                u = unit.name,
            ))
            .collect::<Vec<_>>()
            .join(" ")
    );
    Chart {
        name: "send-latency",
        alt: "Time to send a payment by shape: a warm send, plus generator setup on a first send"
            .into(),
        title: title.into(),
        desc,
        height,
        body: body.0,
    }
}

fn send_breakdown(results: &Results) -> Chart {
    let title = "Where a warm send spends its time, one core";
    let sub = "Proving against everything else a send does.";
    let unit = TimeUnit::for_max(
        results
            .shapes
            .iter()
            .map(ShapeResult::warm_send_ns)
            .fold(0.0, f64::max),
    );
    let parts = [
        square("s1", "Proving (estimated)"),
        square("s2", "Prover run and sealing"),
        square("s3", "Prepare, sign and package"),
    ];
    let rows: Vec<BarRow> = results
        .shapes
        .iter()
        .map(|shape| {
            let values = [
                shape.proving_ns(),
                shape.build_other_ns(),
                shape.around_build_ns(),
            ];
            BarRow {
                label: shape.label.clone(),
                tips: values
                    .iter()
                    .zip(&parts)
                    .map(|(value, key)| {
                        format!(
                            "{}: {} {} {}",
                            shape.label,
                            key.label,
                            unit.format(*value),
                            unit.name
                        )
                    })
                    .collect(),
                values: values.iter().map(|ns| ns / unit.ns).collect(),
                value: format!(
                    "{} {} · {:.0}% proving",
                    unit.format(shape.warm_send_ns()),
                    unit.name,
                    shape.proving_share() * 100.0
                ),
            }
        })
        .collect();

    let mut notes = vec![
        "Proving: a synthetic proof with the same gate count. Sealing: outputs × opening one \
         note."
            .to_owned(),
    ];
    if let Some(widest) = results.shapes.iter().max_by_key(|shape| shape.outputs) {
        notes.push(format!(
            "Sealing grows with outputs: {} notes take an estimated {} {} of the {} send.",
            widest.outputs,
            unit.format(widest.sealing_estimate_ns()),
            unit.name,
            widest.label,
        ));
    }
    let flagged: Vec<&str> = results
        .shapes
        .iter()
        .filter(|shape| shape.send_estimate_overshot())
        .map(|shape| shape.label.as_str())
        .collect();
    if !flagged.is_empty() {
        notes.push(format!(
            "Flagged: the estimates exceed build for {}; the prover run is clamped to zero.",
            and_list(&flagged)
        ));
    }

    let mut body = Body::default();
    let legend_y = heading(&mut body, title, sub);
    let legend_bottom = body.legend(PAD, legend_y, &parts);
    let (base_y, ..) = Bars {
        top: legend_bottom + 16.0,
        left: 84.0,
        rows: &rows,
        classes: &["s1", "s2", "s3", "s4"],
        unit: unit.name,
        value_class: "value",
        min_max: 0.0,
    }
    .draw(&mut body);
    let height = finish(&mut body, base_y, &notes);

    let desc = format!(
        "{sub} {}",
        results
            .shapes
            .iter()
            .map(|shape| format!(
                "{}: {} {u} in all, proving {} {u}, prover run and sealing {} {u}, prepare, sign \
                 and package {} {u}.",
                shape.label,
                unit.format(shape.warm_send_ns()),
                unit.format(shape.proving_ns()),
                unit.format(shape.build_other_ns()),
                unit.format(shape.around_build_ns()),
                u = unit.name,
            ))
            .collect::<Vec<_>>()
            .join(" ")
    );
    Chart {
        name: "send-breakdown",
        alt: "Where a warm send spends its time, by shape".into(),
        title: title.into(),
        desc,
        height,
        body: body.0,
    }
}

/// About how many gates one more output adds: the slope between the
/// single-input shapes with the fewest and the most outputs.
pub fn gates_per_output(results: &Results) -> Option<f64> {
    let single: Vec<&ShapeResult> = results
        .shapes
        .iter()
        .filter(|shape| shape.inputs == 1)
        .collect();
    let few = single.iter().min_by_key(|shape| shape.outputs)?;
    let many = single.iter().max_by_key(|shape| shape.outputs)?;
    (many.outputs > few.outputs).then(|| {
        (many.multiplications as f64 - few.multiplications as f64)
            / (many.outputs - few.outputs) as f64
    })
}

fn proof_capacity(results: &Results) -> Chart {
    let limit = results.limits.max_multiplications_per_transaction;
    let title = format!(
        "Proof capacity used, out of {} gates",
        thousands(limit as u64)
    );
    let sub = "Multiplication gates each shape's proof needs.";
    // A transfer tried past the shapes is drawn as what it did: one that
    // proved is a bar like a shape's, and only one that failed is critical.
    enum Row<'a> {
        Proves {
            label: &'a str,
            gates: Option<usize>,
        },
        Fails(&'a CapacityAttempt),
    }
    let rows: Vec<Row> = results
        .shapes
        .iter()
        .map(|shape| Row::Proves {
            label: &shape.label,
            gates: Some(shape.multiplications),
        })
        .chain(results.over_capacity.iter().map(|attempt| {
            if attempt.proves {
                Row::Proves {
                    label: &attempt.label,
                    gates: attempt.multiplications,
                }
            } else {
                Row::Fails(attempt)
            }
        }))
        .collect();

    let mut body = Body::default();
    let _ = heading(&mut body, &title, sub);
    let left = 84.0;
    let right = 150.0;
    let top = 96.0;
    let plot_w = WIDTH - left - right;
    let most = rows
        .iter()
        .filter_map(|row| match row {
            Row::Proves { gates, .. } => *gates,
            Row::Fails(attempt) => attempt.multiplications,
        })
        .fold(limit, usize::max);
    let axis = Axis::nice(most as f64 * 1.1, 6.0);
    let sx = |v: f64| left + v / axis.max * plot_w;
    let base_y = top + ROW * rows.len() as f64;

    for tick in axis.ticks() {
        let x = sx(tick);
        body.line(x, top - 6.0, x, base_y, "grid");
        body.text(x, base_y + 16.0, &axis.label(tick), "tick", Anchor::Middle);
    }
    body.text(left + plot_w, base_y + 32.0, "gates", "note", Anchor::End);
    body.line(left, top - 6.0, left, base_y, "axis");
    let limit_x = sx(limit as f64);
    let mut desc_rows = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let y = top + index as f64 * ROW + (ROW - BAR) / 2.0;
        let text_y = y + BAR / 2.0 + 4.0;
        match row {
            Row::Proves { label, gates: None } => {
                // It proved, but its gates were not recorded: no bar.
                body.text(left - 12.0, text_y, label, "label", Anchor::End);
                body.text(left + 8.0, text_y, "proves", "value", Anchor::Start);
                desc_rows.push(format!("{label}: proves."));
            }
            Row::Proves {
                label,
                gates: Some(gates),
            } => {
                let gates = *gates;
                let x = sx(gates as f64);
                let value = format!(
                    "{} · {:.0}%",
                    thousands(gates as u64),
                    gates as f64 / limit as f64 * 100.0
                );
                body.text(left - 12.0, text_y, label, "label", Anchor::End);
                body.hbar(
                    left,
                    y,
                    x,
                    "s1",
                    true,
                    &format!(
                        "{label}: {} of {} gates",
                        thousands(gates as u64),
                        thousands(limit as u64)
                    ),
                );
                // A label never sits on the limit line: one that would
                // cross it starts past it.
                let crosses = x + 8.0 - 4.0 < limit_x
                    && limit_x < x + 8.0 + text_width(&value, "value") + 4.0;
                let label_x = if crosses {
                    x.max(limit_x) + 8.0
                } else {
                    x + 8.0
                };
                body.text(label_x, text_y, &value, "value", Anchor::Start);
                desc_rows.push(format!("{label}: {value}."));
            }
            Row::Fails(attempt) => {
                // Without an observed count the bar runs to the limit line
                // and carries no value.
                let end = attempt
                    .multiplications
                    .map_or(limit_x, |gates| sx(gates as f64));
                let tip = match attempt.multiplications {
                    Some(gates) => format!(
                        "{}: {} gates, over the limit; does not prove",
                        attempt.label,
                        thousands(gates as u64)
                    ),
                    None => format!("{}: does not prove", attempt.label),
                };
                body.text(left - 12.0, text_y, &attempt.label, "label", Anchor::End);
                body.hbar(left, y, end, "crit", true, &tip);
                body.text(
                    end.max(limit_x) + 8.0,
                    text_y,
                    "✕ does not prove",
                    "crit-ink",
                    Anchor::Start,
                );
                desc_rows.push(format!("{}: does not prove.", attempt.label));
            }
        }
    }
    body.line_titled(
        limit_x,
        top - 10.0,
        limit_x,
        base_y,
        "ref",
        &format!("The limit: {} gates per proof", thousands(limit as u64)),
    );
    body.text(
        limit_x,
        top - 14.0,
        &format!("limit {}", thousands(limit as u64)),
        "note",
        Anchor::Middle,
    );
    let fits = results.most_outputs().unwrap_or(0);
    let limit_note = match results.fewest_failing_outputs() {
        Some(fails) => format!("{fits} outputs fit, {fails} do not."),
        None => format!("{fits} outputs fit."),
    };
    let notes = [match gates_per_output(results) {
        Some(slope) => format!("About {slope:.0} gates per output: {limit_note}"),
        None => limit_note,
    }];
    let height = finish(&mut body, base_y, &notes);

    Chart {
        name: "proof-capacity",
        alt: "Proof capacity used by shape, against the 1,024-gate limit".into(),
        desc: format!("{sub} {}", desc_rows.join(" ")),
        title,
        height,
        body: body.0,
    }
}

/// Decimal megabytes with one decimal: 1 MB is 1,000,000 bytes.
pub fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

/// Decimal kilobytes with one decimal: 1 KB is 1,000 bytes.
pub fn kilobytes(bytes: u64) -> String {
    format!("{:.1} KB", bytes as f64 / 1e3)
}

fn peak_memory(results: &Results) -> Chart {
    let title = "Peak memory of a first send";
    let sub =
        "Heap in use at its highest during a send in a new process, generator table included.";
    let rows: Vec<BarRow> = results
        .shapes
        .iter()
        .map(|shape| {
            let bytes = shape.send.fresh.first_peak_bytes;
            BarRow {
                label: shape.label.clone(),
                values: vec![bytes as f64 / 1e6],
                tips: vec![format!(
                    "{}: {} ({} bytes)",
                    shape.label,
                    megabytes(bytes),
                    thousands(bytes)
                )],
                value: megabytes(bytes),
            }
        })
        .collect();

    let mut body = Body::default();
    let _ = heading(&mut body, title, sub);
    let (base_y, ..) = Bars {
        top: 84.0,
        left: 84.0,
        rows: &rows,
        classes: &["s1"],
        unit: "MB",
        value_class: "value",
        min_max: 0.0,
    }
    .draw(&mut body);
    let notes = [
        "A counting allocator, from the start of the send to its end. A warm send, with the \
         table already built, needs less."
            .to_owned(),
    ];
    let height = finish(&mut body, base_y, &notes);

    let desc = format!(
        "{sub} {}",
        rows.iter()
            .map(|row| format!("{}: {}.", row.label, row.value))
            .collect::<Vec<_>>()
            .join(" "),
    );
    Chart {
        name: "peak-memory",
        alt: "Peak memory of a first send, by shape".into(),
        title: title.into(),
        desc,
        height,
        body: body.0,
    }
}

/// The frame both scaling charts share: evenly spaced thread counts, the
/// shaded regions with their labels on top, and the x axis.
struct ThreadFrame {
    xs: Vec<f64>,
    top: f64,
    base_y: f64,
    left: f64,
    plot_w: f64,
}

/// A run of adjacent thread-count intervals that end in the same region.
struct Band {
    region: Region,
    from: usize,
    to: usize,
}

/// The regions between measured thread counts: the interval from one count
/// to the next belongs to the region of the CPUs it adds.
fn bands(scaling: &Scaling) -> Vec<Band> {
    let points = &scaling.points;
    let mut bands: Vec<Band> = Vec::new();
    for (index, point) in points.iter().enumerate().skip(1) {
        let Some(region) = scaling.region(point.threads) else {
            continue;
        };
        match bands.last_mut() {
            Some(band) if band.region == region && band.to == index - 1 => band.to = index,
            _ => bands.push(Band {
                region,
                from: index - 1,
                to: index,
            }),
        }
    }
    bands
}

impl ThreadFrame {
    /// Lays out the plot between `legend_bottom` and `base_y`, shades the
    /// regions and draws the x axis.
    fn new(scaling: &Scaling, body: &mut Body, legend_bottom: f64, base_y: f64) -> ThreadFrame {
        let (left, right) = (72.0, 170.0);
        let plot_w = WIDTH - left - right;
        let points = &scaling.points;
        let slots = points.len().saturating_sub(1).max(1) as f64;
        let xs: Vec<f64> = (0..points.len())
            .map(|i| left + i as f64 / slots * plot_w)
            .collect();

        // Region labels sit in a strip at the top of their band.
        let bands = bands(scaling);
        let labels: Vec<Vec<String>> = bands
            .iter()
            .map(|band| {
                let width = xs[band.to] - xs[band.from] - 8.0;
                wrap(band.region.label(), "note", width)
            })
            .collect();
        let strip_lines = labels.iter().map(Vec::len).max().unwrap_or(0) as f64;
        let band_top = legend_bottom + 14.0;
        let top = band_top + strip_lines * LINE + 12.0;
        for (index, (band, lines)) in bands.iter().zip(&labels).enumerate() {
            let (x0, x1) = (xs[band.from], xs[band.to]);
            // A 2 px surface gap between neighbouring regions.
            let gap_left = if index > 0 { 1.0 } else { 0.0 };
            let gap_right = if index + 1 < bands.len() { 1.0 } else { 0.0 };
            let cpus: Vec<String> = scaling.cpus_used(points[band.to].threads)
                [points[band.from].threads..]
                .iter()
                .map(|cpu| cpu.cpu.to_string())
                .collect();
            let tip = format!(
                "{}: threads {}–{} add CPUs {}",
                band.region.label(),
                points[band.from].threads + 1,
                points[band.to].threads,
                cpus.join(", ")
            );
            body.band(
                x0 + gap_left,
                band_top,
                x1 - x0 - gap_left - gap_right,
                base_y - band_top,
                index % 2 == 1,
                &tip,
            );
            for (line_index, line) in lines.iter().enumerate() {
                body.text(
                    (x0 + x1) / 2.0,
                    band_top + LINE * (line_index as f64 + 1.0),
                    line,
                    "note",
                    Anchor::Middle,
                );
            }
        }
        body.line(left, base_y, left + plot_w, base_y, "axis");
        for (x, point) in xs.iter().zip(points) {
            body.text(
                *x,
                base_y + 18.0,
                &point.threads.to_string(),
                "tick",
                Anchor::Middle,
            );
        }
        body.text(
            left + plot_w / 2.0,
            base_y + 36.0,
            "worker threads",
            "note",
            Anchor::Middle,
        );
        ThreadFrame {
            xs,
            top,
            base_y,
            left,
            plot_w,
        }
    }

    fn sy(&self, value: f64, max: f64) -> f64 {
        self.base_y - value / max * (self.base_y - self.top)
    }

    /// Horizontal gridlines with tick labels on the left.
    fn grid(&self, body: &mut Body, ticks: &[(f64, String)], max: f64) {
        for (value, label) in ticks {
            let y = self.sy(*value, max);
            if *value > 0.0 {
                body.line(self.left, y, self.left + self.plot_w, y, "grid");
            }
            body.text(self.left - 10.0, y + 4.0, label, "tick", Anchor::End);
        }
    }

    fn right(&self) -> f64 {
        self.left + self.plot_w
    }
}

/// The x axis of both scaling charts.
const SCALING_BASE: f64 = 340.0;

fn verifier_scaling(scaling: &Scaling) -> Chart {
    let title = "Verifier scaling, not node TPS";
    let sub = format!(
        "{} transactions verified per second by worker thread count, {} per iteration.",
        scaling.shape, scaling.pool
    );
    let points = &scaling.points;
    let base = scaling.one_thread_tps().expect("validated");
    let ideal: Vec<f64> = points.iter().map(|p| base * p.threads as f64).collect();
    let notes = ["Today the node verifies on one thread.".to_owned()];

    let mut body = Body::default();
    let legend_y = heading(&mut body, title, &sub);
    let legend_bottom = body.legend(
        PAD,
        legend_y,
        &[
            line_key("l1", "Measured"),
            line_key("ideal", "Ideal (threads × one thread)"),
        ],
    );
    let frame = ThreadFrame::new(scaling, &mut body, legend_bottom, SCALING_BASE);
    let top_value = ideal
        .iter()
        .chain(points.iter().map(|p| &p.tps))
        .fold(0.0f64, |a, b| a.max(*b));
    let axis = Axis::nice(top_value, 5.0);
    let ticks: Vec<(f64, String)> = axis
        .ticks()
        .into_iter()
        .map(|tick| (tick, axis.label(tick)))
        .collect();
    frame.grid(&mut body, &ticks, axis.max);

    let ideal_points: Vec<(f64, f64)> = frame
        .xs
        .iter()
        .zip(&ideal)
        .map(|(x, v)| (*x, frame.sy(*v, axis.max)))
        .collect();
    body.polyline(
        &ideal_points,
        "ideal",
        &format!("Ideal: threads × {} TPS", whole(base)),
    );
    let measured: Vec<(f64, f64)> = frame
        .xs
        .iter()
        .zip(points)
        .map(|(x, p)| (*x, frame.sy(p.tps, axis.max)))
        .collect();
    body.polyline(&measured, "l1", "Measured TPS");
    for ((x, y), point) in measured.iter().zip(points) {
        body.dot(
            *x,
            *y,
            "dot1",
            &format!(
                "{} threads: {} TPS ({:.1}x)",
                point.threads,
                whole(point.tps),
                scaling.speedup(point)
            ),
        );
    }

    // Line-end labels, kept apart.
    let last = points.last().expect("validated");
    let last_ideal = *ideal.last().expect("validated");
    let wanted = [
        (frame.sy(last_ideal, axis.max) + 4.0, LINE),
        (frame.sy(last.tps, axis.max) + 4.0, 2.0 * LINE + 2.0),
    ];
    let placed = spread(&wanted, frame.top, frame.base_y + 4.0);
    let x = frame.right() + 10.0;
    body.text(
        x,
        placed[0],
        &format!("ideal {}", whole(last_ideal)),
        "note",
        Anchor::Start,
    );
    body.text(
        x,
        placed[1],
        &format!("{} TPS", whole(last.tps)),
        "value",
        Anchor::Start,
    );
    body.text(
        x,
        placed[1] + LINE + 2.0,
        &format!("{:.1}x on {}", scaling.speedup(last), last.threads),
        "note",
        Anchor::Start,
    );
    let notes_h = body.notes(frame.base_y + 44.0, &notes);
    let height = (frame.base_y + 44.0 + notes_h + 14.0).ceil();

    let desc = format!(
        "{sub} {} Ideal on {} threads: {} TPS.",
        points
            .iter()
            .map(|p| format!(
                "{} threads: {} TPS ({:.1}x).",
                p.threads,
                whole(p.tps),
                scaling.speedup(p)
            ))
            .collect::<Vec<_>>()
            .join(" "),
        last.threads,
        whole(last_ideal),
    );
    Chart {
        name: "verifier-scaling",
        alt: "Verifier throughput by worker thread count, against ideal scaling".into(),
        title: title.into(),
        desc,
        height,
        body: body.0,
    }
}

fn verifier_efficiency(scaling: &Scaling) -> Chart {
    let title = "Verifier efficiency per thread";
    let sub = "Speed-up over one thread, divided by the thread count.";
    let points = &scaling.points;
    let efficiency: Vec<f64> = points
        .iter()
        .map(|p| scaling.efficiency(p) * 100.0)
        .collect();
    let notes = [
        "Threads join fast cores first, then compact cores, then second threads of busy cores."
            .to_owned(),
    ];

    let mut body = Body::default();
    let legend_y = heading(&mut body, title, sub);
    let legend_bottom = body.legend(
        PAD,
        legend_y,
        &[line_key("l1", "Measured"), line_key("ideal", "Ideal")],
    );
    let frame = ThreadFrame::new(scaling, &mut body, legend_bottom, SCALING_BASE);
    let max = efficiency
        .iter()
        .fold(110.0f64, |a, b| a.max((b / 10.0).ceil() * 10.0 + 10.0));
    let ticks: Vec<(f64, String)> = (0..)
        .map(|i| i as f64 * 25.0)
        .take_while(|v| *v <= max)
        .map(|v| (v, format!("{v:.0}%")))
        .collect();
    frame.grid(&mut body, &ticks, max);

    let y100 = frame.sy(100.0, max);
    body.line_titled(
        frame.left,
        y100,
        frame.right(),
        y100,
        "ideal",
        "Ideal: 100% of one thread's rate per thread",
    );
    let measured: Vec<(f64, f64)> = frame
        .xs
        .iter()
        .zip(&efficiency)
        .map(|(x, e)| (*x, frame.sy(*e, max)))
        .collect();
    body.polyline(&measured, "l1", "Measured efficiency");
    for (((x, y), point), e) in measured.iter().zip(points).zip(&efficiency) {
        body.dot(
            *x,
            *y,
            "dot1",
            &format!(
                "{} threads: {e:.0}% ({} TPS, {:.1}x)",
                point.threads,
                whole(point.tps),
                scaling.speedup(point)
            ),
        );
    }

    // Direct labels on the region boundaries: the last count of each
    // region. The final one sits right of the line's end, the others on
    // the side the line does not come from.
    let bands = bands(scaling);
    let last = points.len() - 1;
    let mut right_labels: Vec<(f64, f64)> = vec![(y100 + 4.0, LINE)];
    for band in &bands {
        let index = band.to;
        let (x, y) = measured[index];
        let label = format!("{:.0}% on {}", efficiency[index], points[index].threads);
        if index == last {
            right_labels.push((y + 4.0, 2.0 * LINE + 2.0));
            continue;
        }
        let from_above = index > 0 && efficiency[index - 1] > efficiency[index];
        let ly = if from_above { y + 20.0 } else { y - 12.0 };
        body.text(x - 8.0, ly, &label, "value", Anchor::End);
    }
    let placed = spread(&right_labels, frame.top, frame.base_y + 4.0);
    let x = frame.right() + 10.0;
    body.text(x, placed[0], "ideal 100%", "note", Anchor::Start);
    if right_labels.len() > 1 {
        let point = &points[last];
        body.text(
            x,
            placed[1],
            &format!("{:.0}% on {}", efficiency[last], point.threads),
            "value",
            Anchor::Start,
        );
        body.text(
            x,
            placed[1] + LINE + 2.0,
            &format!("{:.1}x of {}x", scaling.speedup(point), point.threads),
            "note",
            Anchor::Start,
        );
    }
    let notes_h = body.notes(frame.base_y + 44.0, &notes);
    let height = (frame.base_y + 44.0 + notes_h + 14.0).ceil();

    let desc = format!(
        "{sub} {}",
        points
            .iter()
            .zip(&efficiency)
            .map(|(p, e)| format!(
                "{} threads: {e:.0}%{}.",
                p.threads,
                scaling
                    .region(p.threads)
                    .map_or_else(String::new, |r| format!(", {}", r.label()))
            ))
            .collect::<Vec<_>>()
            .join(" ")
    );
    Chart {
        name: "verifier-efficiency",
        alt: "Verifier efficiency per thread by worker thread count".into(),
        title: title.into(),
        desc,
        height,
        body: body.0,
    }
}

/// `a`, `a and b`, or `a, b and c`.
pub fn and_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A time in the unit that reads best for it alone.
pub fn format_time(ns: f64) -> String {
    let unit = TimeUnit::for_max(ns);
    format!("{} {}", unit.format(ns), unit.name)
}
