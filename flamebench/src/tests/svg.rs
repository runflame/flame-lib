//! The charts: accessible, complete, finite, and one drawing per theme.

use super::{sample_results, sample_scaling};
use crate::results::Results;
use crate::svg::{charts, scaling_charts, Chart, Theme};

fn chart<'a>(charts: &'a [Chart], name: &str) -> &'a Chart {
    charts
        .iter()
        .find(|chart| chart.name == name)
        .expect("the chart is drawn")
}

fn count(svg: &str, needle: &str) -> usize {
    svg.matches(needle).count()
}

fn check(charts: &[Chart]) {
    for chart in charts {
        let light = chart.render(Theme::Light);
        let dark = chart.render(Theme::Dark);
        for svg in [&light, &dark] {
            assert!(svg.contains("role=\"img\""), "{}", chart.name);
            assert_eq!(count(svg, "<title id=\"t\">"), 1, "{}", chart.name);
            assert_eq!(count(svg, "<desc id=\"d\">"), 1, "{}", chart.name);
            assert!(svg.contains("@media (prefers-color-scheme: dark)"));
            for bad in ["NaN", "inf"] {
                assert!(!svg.contains(bad), "{} holds {bad}", chart.name);
            }
            // Every mark carries its exact value.
            let marks = count(svg, "class=\"bar ")
                + count(svg, "class=\"dot ")
                + count(svg, "class=\"mark ")
                + count(svg, "class=\"band");
            assert!(
                count(svg, "<title>") >= marks,
                "{}: a mark without a title",
                chart.name
            );
        }
        assert!(light.contains("data-theme=\"light\""));
        assert!(dark.contains("data-theme=\"dark\""));
        assert_eq!(
            dark.replacen("data-theme=\"dark\"", "data-theme=\"light\"", 1),
            light,
            "{}: the dark file differs only in data-theme",
            chart.name
        );
    }
}

fn check_all(results: &Results) {
    let charts = charts(results);
    assert_eq!(charts.len(), 7);
    check(&charts);
}

#[test]
fn every_chart_is_accessible_finite_and_themed() {
    check_all(&sample_results());
    let scaling = scaling_charts(&sample_scaling());
    assert_eq!(scaling.len(), 2);
    check(&scaling);
}

#[test]
fn every_chart_has_its_marks() {
    let results = sample_results();
    let charts = charts(&results);
    let shapes = results.shapes.len();

    let cost = chart(&charts, "cost-by-shape").render(Theme::Light);
    assert_eq!(
        count(&cost, "class=\"bar "),
        shapes * 4,
        "four segments a shape: proof, VM and other, signature, Utreexo"
    );
    assert_eq!(count(&cost, "class=\"key "), 4, "a legend key a segment");
    assert_eq!(
        count(&cost, "class=\"bar s4\""),
        shapes,
        "Utreexo is its own segment"
    );

    let tps = chart(&charts, "tps-per-core").render(Theme::Light);
    assert_eq!(count(&tps, "class=\"bar s1\""), shapes);
    assert!(tps.contains("One core&#x27;s verification rate"));
    assert!(!tps.contains("class=\"ref\""), "no reference line");

    let gas = chart(&charts, "gas-check").render(Theme::Light);
    assert_eq!(
        count(&gas, "class=\"mark s1\""),
        shapes,
        "a measurement a shape"
    );
    for dot in ["dot2", "dot3"] {
        assert_eq!(
            count(&gas, &format!("class=\"dot {dot}\"")),
            shapes,
            "{dot}"
        );
    }
    assert!(gas.contains("today −72% · per gate −7%"), "{gas}");

    let latency = chart(&charts, "send-latency").render(Theme::Light);
    assert_eq!(count(&latency, "class=\"bar "), shapes * 2);
    assert!(latency.contains("14.7 warm + 31.7 setup"), "{latency}");

    let breakdown = chart(&charts, "send-breakdown").render(Theme::Light);
    assert_eq!(count(&breakdown, "class=\"bar "), shapes * 3);
    assert!(breakdown.contains("13 notes take an estimated"));

    let memory = chart(&charts, "peak-memory").render(Theme::Light);
    assert_eq!(count(&memory, "class=\"bar s1\""), shapes);
    assert!(memory.contains("1.4 MB"));

    for name in ["verifier-scaling", "verifier-efficiency"] {
        let scaling = sample_scaling();
        let charts = scaling_charts(&scaling);
        let svg = chart(&charts, name).render(Theme::Light);
        assert_eq!(
            count(&svg, "class=\"dot dot1\""),
            scaling.points.len(),
            "{name} dots"
        );
        assert_eq!(count(&svg, "class=\"band"), 3, "{name} regions");
        for region in ["fast cores", "compact cores", "second hardware"] {
            assert!(svg.contains(region), "{name} labels {region}");
        }
        assert!(!svg.contains("ceiling"), "{name} has no ceiling");
    }
}

#[test]
fn a_transfer_that_does_not_prove_is_drawn_to_the_limit_without_a_value() {
    let results = sample_results();
    let charts = charts(&results);
    let svg = chart(&charts, "proof-capacity").render(Theme::Light);
    assert_eq!(count(&svg, "class=\"bar s1\""), results.shapes.len());
    assert_eq!(count(&svg, "class=\"bar crit\""), 1);
    // Status never by color alone: an icon and a label ride with it.
    assert_eq!(count(&svg, "✕ does not prove"), 1);
    assert!(svg.contains("<title>1 → 14: does not prove</title>"));
    assert!(svg.contains("limit 1,024"));
    assert!(svg.contains("137 · 13%"));
    assert!(svg.contains("885 · 86%"));
    // Its label would cross the limit line, so it starts past it.
    let x_of = |needle: &str| -> f64 {
        // The text element, not the description.
        let at = svg.find(&format!(">{needle}<")).expect("the label");
        let before = &svg[..at];
        let x = before.rsplit("<text x=\"").next().expect("a text element");
        x.split('"').next().unwrap().parse().expect("a number")
    };
    let limit_x: f64 = svg
        .split("class=\"ref\"")
        .next()
        .and_then(|before| before.rsplit("x1=\"").next())
        .and_then(|rest| rest.split('"').next())
        .expect("the limit line")
        .parse()
        .expect("a number");
    assert!(x_of("885 · 86%") > limit_x, "past the limit line");
    assert!(
        x_of("137 · 13%") < limit_x,
        "short bars keep theirs by the bar"
    );
    assert!(svg.contains("About 68 gates per output: 13 outputs fit, 14 do not."));

    // The bar ends at the limit line: the same x.
    let limit_x = svg
        .split("class=\"ref\"")
        .next()
        .and_then(|before| before.rsplit("x1=\"").next())
        .and_then(|rest| rest.split('"').next())
        .expect("the limit line")
        .to_owned();
    let crit = svg
        .split("class=\"bar crit\"")
        .next()
        .and_then(|before| before.rsplit("<path d=\"").next())
        .expect("the critical bar")
        .to_owned();
    assert!(
        crit.contains(&format!(",{}", limit_x)) || crit.contains(&format!(" {limit_x},")),
        "the bar reaches x = {limit_x}: {crit}"
    );

    // An observed count draws the bar to it.
    let mut observed = results.clone();
    observed.over_capacity[0].multiplications = Some(1_081);
    let charts = crate::svg::charts(&observed);
    let svg = chart(&charts, "proof-capacity").render(Theme::Light);
    assert!(svg.contains("1 → 14: 1,081 gates, over the limit; does not prove"));
}

#[test]
fn a_transfer_past_the_shapes_that_proves_is_drawn_as_one() {
    let mut results = sample_results();
    results.over_capacity[0].proves = true;
    results.over_capacity[0].error = None;
    results.over_capacity[0].multiplications = Some(1_010);
    assert_eq!(results.most_outputs(), Some(14));
    assert_eq!(results.fewest_failing_outputs(), None);
    let charts = charts(&results);
    let svg = chart(&charts, "proof-capacity").render(Theme::Light);
    assert_eq!(count(&svg, "class=\"bar crit\""), 0);
    assert!(!svg.contains("does not prove"));
    assert_eq!(count(&svg, "class=\"bar s1\""), results.shapes.len() + 1);
    assert!(svg.contains(">1,010 · 99%<"));
    assert!(!svg.contains("13 outputs fit"));
    assert!(svg.contains("14 outputs fit."));

    // Proved, but without a recorded count: named, never drawn to a value.
    results.over_capacity[0].multiplications = None;
    let charts = crate::svg::charts(&results);
    let svg = chart(&charts, "proof-capacity").render(Theme::Light);
    assert_eq!(count(&svg, "class=\"bar s1\""), results.shapes.len());
    assert!(svg.contains(">proves<"));
}

#[test]
fn zero_and_extreme_numbers_stay_finite() {
    let mut results = sample_results();
    results.shapes[0].r1cs_estimate_ns = 0.0;
    results.shapes[1].signature_ns = results.shapes[1].verify_ns * 2.0;
    results.shapes[2].gas_used = 0;
    results.shapes[3].send.prove_estimate_ns = results.shapes[3].send.build_ns * 2.0;
    results.shapes[0].send.fresh.first_ns = 0.0;
    results.shapes[1].send.fresh.first_peak_bytes = 0;
    check_all(&results);
    let mut scaling = sample_scaling();
    scaling.points[4].tps = 0.0;
    check(&scaling_charts(&scaling));
}

#[test]
fn fewer_regions_on_simpler_machines() {
    let mut scaling = sample_scaling();
    // Six cores without second threads: fast, then compact.
    let topology: Vec<_> = (0..6)
        .map(|cpu| super::logical(cpu, cpu, &[cpu], if cpu < 4 { 4_800 } else { 3_000 }))
        .collect();
    scaling.machine.cpu_order = crate::cpu::order(&topology);
    scaling.points.truncate(3);
    let charts = scaling_charts(&scaling);
    let svg = chart(&charts, "verifier-scaling").render(Theme::Light);
    assert_eq!(
        count(&svg, "class=\"band"),
        1,
        "only fast cores between 1 and 4"
    );
    assert!(!svg.contains("second hardware"));
}
