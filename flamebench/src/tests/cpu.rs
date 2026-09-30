//! The CPU order, from hand-written topologies only.

use super::{logical, this_machine};
use crate::cpu::{cpu_slug, order, parse_cpu_list, region, CoreClass, Region};

fn cpus(order: &[crate::cpu::Cpu]) -> Vec<usize> {
    order.iter().map(|cpu| cpu.cpu).collect()
}

#[test]
fn this_machine_orders_fast_then_compact_then_second_threads() {
    let order = order(&this_machine());
    assert_eq!(
        cpus(&order),
        [2, 4, 6, 0, 1, 3, 5, 7, 10, 12, 14, 8, 9, 11, 13, 15]
    );
    let regions: Vec<Region> = (1..=16)
        .map(|threads| region(&order, threads).expect("a region"))
        .collect();
    assert_eq!(&regions[..4], [Region::FastCores; 4]);
    assert_eq!(&regions[4..8], [Region::CompactCores; 4]);
    assert_eq!(&regions[8..], [Region::SecondThreads; 8]);
    assert_eq!(region(&order, 0), None);
    assert_eq!(region(&order, 17), None);

    // The seventh core is a little slower still, and compact all the same.
    let cpu7 = order.iter().find(|cpu| cpu.cpu == 7).expect("CPU 7");
    assert_eq!(cpu7.class, CoreClass::Compact);
    let cpu10 = order.iter().find(|cpu| cpu.cpu == 10).expect("CPU 10");
    assert_eq!(
        (cpu10.core, cpu10.thread, cpu10.class),
        (2, 1, CoreClass::Fast)
    );
}

#[test]
fn an_all_equal_machine_has_no_compact_region() {
    // Four cores within 5% of each other, two threads each.
    let topology: Vec<_> = (0..8)
        .map(|cpu| {
            let core = cpu % 4;
            logical(cpu, core, &[core, core + 4], 4_000 - 50 * core as u64)
        })
        .collect();
    let order = order(&topology);
    assert_eq!(cpus(&order), [1, 2, 3, 0, 5, 6, 7, 4]);
    assert!(order.iter().all(|cpu| cpu.class == CoreClass::Fast));
    let regions: Vec<_> = (1..=8).filter_map(|n| region(&order, n)).collect();
    assert!(!regions.contains(&Region::CompactCores));
    assert_eq!(regions[..4], [Region::FastCores; 4]);
    assert_eq!(regions[4..], [Region::SecondThreads; 4]);
}

#[test]
fn a_machine_without_second_threads_has_no_smt_region() {
    let topology: Vec<_> = (0..6)
        .map(|cpu| logical(cpu, cpu, &[cpu], if cpu < 2 { 4_800 } else { 3_000 }))
        .collect();
    let order = order(&topology);
    assert_eq!(cpus(&order), [1, 0, 2, 3, 4, 5]);
    let regions: Vec<_> = (1..=6).filter_map(|n| region(&order, n)).collect();
    assert!(!regions.contains(&Region::SecondThreads));
    assert_eq!(regions[..2], [Region::FastCores; 2]);
    assert_eq!(regions[2..], [Region::CompactCores; 4]);
}

#[test]
fn unknown_frequencies_count_as_fast() {
    let mut topology = this_machine();
    for cpu in &mut topology {
        cpu.max_freq_khz = None;
    }
    let order = order(&topology);
    assert!(order.iter().all(|cpu| cpu.class == CoreClass::Fast));
    assert_eq!(
        cpus(&order),
        [1, 2, 3, 4, 5, 6, 7, 0, 9, 10, 11, 12, 13, 14, 15, 8]
    );
}

#[test]
fn cpu_lists_parse() {
    assert_eq!(parse_cpu_list("0,8"), Ok(vec![0, 8]));
    assert_eq!(parse_cpu_list("0-3,8-9,12"), Ok(vec![0, 1, 2, 3, 8, 9, 12]));
    assert!(parse_cpu_list("0-x").is_err());
}

#[test]
fn cpu_slugs_drop_the_vendor_and_the_tail() {
    assert_eq!(
        cpu_slug("AMD Ryzen AI 7 350 w/ Radeon 860M"),
        "ryzen-ai-7-350"
    );
    assert_eq!(
        cpu_slug("Intel(R) Core(TM) i7-8650U CPU @ 1.90GHz"),
        "core-i7-8650u-cpu-1-90ghz"
    );
    assert_eq!(cpu_slug("Apple M2 Pro"), "m2-pro");
    assert_eq!(cpu_slug(""), "unknown-cpu");
}

#[test]
fn only_the_documented_command_is_a_full_run() {
    let run = |args: &[&str]| {
        crate::full_run_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    };
    assert!(run(&["--bench"]), "cargo bench passes exactly this");
    assert!(!run(&[]), "not started by cargo bench");
    assert!(!run(&["--bench", "send/sign"]), "a filter");
    assert!(!run(&["--bench", "--list"]));
    assert!(!run(&[
        "--bench",
        "--profile-time",
        "10",
        "tx_cost/verify/1to13"
    ]));
    assert!(!run(&["--test"]), "criterion's test mode");
    assert!(!run(&["--bench", "--noplot"]), "any other option");
}

#[test]
fn the_benchmarks_must_be_pinned_to_the_first_cpu() {
    let order = order(&this_machine());
    assert_eq!(crate::pinned_cpu(&[2], &order), Ok(2));
    for allowed in [&[4][..], &[2, 4], &(0..16).collect::<Vec<_>>(), &[]] {
        let error = crate::pinned_cpu(allowed, &order).expect_err("not pinned to CPU 2");
        assert!(error.contains("taskset -c 2 cargo bench"), "{error}");
    }
    assert!(crate::pinned_cpu(&[0], &[]).is_err(), "no order");
}
