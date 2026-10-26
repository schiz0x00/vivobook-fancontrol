//! Extract the per-profile EC fan tables from the decompiled DSDT.
//!
//! The profile switch (FANL) calls one of FSMD/FWMD/FPMD/FFMD, each a flat
//! list of `\_SB.PC00.LPCB.EC0.WRAM (addr, value)` calls.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use anyhow::{Context, Result};
use regex::Regex;

pub const PROFILES: &[(&str, &str)] = &[("FSMD", "standard"), ("FWMD", "whisper"), ("FPMD", "perf"), ("FFMD", "full")];

fn method_body<'a>(lines: &'a [&'a str], name: &str) -> Result<&'a [&'a str]> {
    let header = format!("Method ({name},");
    let start = lines.iter().position(|l| l.contains(&header)).with_context(|| format!("method {name} not found"))?;

    let mut depth = 0i32;
    for (end, line) in lines.iter().enumerate().skip(start) {
        depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
        if depth == 0 && end > start + 1 {
            return Ok(&lines[start..=end]);
        }
    }
    anyhow::bail!("method {name} is unterminated")
}

fn wram_writes(body: &[&str]) -> BTreeMap<u16, u8> {
    let re = Regex::new(r"WRAM \(0x([0-9A-F]+), (0x[0-9A-F]+|One|Zero)\)").unwrap();
    body.iter()
        .filter_map(|l| re.captures(l))
        .map(|c| {
            let addr = u16::from_str_radix(&c[1], 16).unwrap();
            let value = match &c[2] {
                "Zero" => 0,
                "One" => 1,
                hex => u8::from_str_radix(&hex[2..], 16).unwrap(),
            };
            (addr, value)
        })
        .collect()
}

/// Side-by-side table of every EC RAM write per profile; `*` marks rows
/// that differ between profiles.
pub fn fan_tables(dsl: &str) -> Result<String> {
    let lines: Vec<&str> = dsl.lines().collect();
    let tables =
        PROFILES.iter().map(|(method, _)| Ok(wram_writes(method_body(&lines, method)?))).collect::<Result<Vec<_>>>()?;
    let addrs: BTreeSet<u16> = tables.iter().flat_map(|t| t.keys().copied()).collect();

    let mut out = String::from("addr  ");
    for (_, name) in PROFILES {
        write!(out, " {name:>9}")?;
    }
    out.push('\n');

    for addr in addrs {
        let row: Vec<Option<u8>> = tables.iter().map(|t| t.get(&addr).copied()).collect();
        write!(out, "0x{addr:04X}")?;
        for v in &row {
            match v {
                Some(v) => write!(out, " {v:>9}")?,
                None => write!(out, " {:>9}", "-")?,
            }
        }
        if row.windows(2).any(|w| w[0] != w[1]) {
            out.push_str(" *");
        }
        out.push('\n');
    }
    Ok(out)
}
