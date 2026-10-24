//! The body of `voom config`: the merged configuration rendered as text.

use std::io;

use crate::catalog::CATALOG;
use crate::config::resolve::Resolved;
use crate::policy::KeepPolicy;

/// Writes the merged configuration, its sources and its deviations from the catalog defaults.
pub(super) fn write_merged(out: &mut dyn io::Write, merged: &Resolved) -> io::Result<()> {
    use owo_colors::OwoColorize;

    writeln!(out, "{}", "sources (ascending precedence)".bold())?;
    if merged.sources.is_empty() {
        writeln!(out, "  (none — built-in defaults only)")?;
    }
    for source in &merged.sources {
        writeln!(out, "  {}", source.display())?;
    }
    writeln!(out, "  {}", "command line".dimmed())?;

    writeln!(out, "\n{}", "[keep]".bold())?;
    write_keep(out, "  ", merged.keep())?;
    for (ecosystem, keep) in merged.keep_overrides() {
        writeln!(out, "  [keep.{ecosystem}]")?;
        write_keep(out, "    ", keep)?;
    }

    writeln!(out, "\n{}", "exclude".bold())?;
    for pattern in &merged.exclude {
        writeln!(out, "  {pattern}")?;
    }
    if merged.exclude.is_empty() {
        writeln!(out, "  (none)")?;
    }

    writeln!(out, "\n{}", "include".bold())?;
    if merged.include.is_empty() {
        writeln!(out, "  (none)")?;
    }
    for pattern in &merged.include {
        writeln!(out, "  {pattern}  {}", "swept without a marker".dimmed())?;
    }

    writeln!(out, "\n{}", "[[paths]]".bold())?;
    let mut any = false;
    for (pattern, keep, ops) in merged.rules() {
        any = true;
        writeln!(out, "  match = \"{pattern}\"")?;
        write_keep(out, "    ", keep)?;
        for (spec, enabled) in ops {
            writeln!(out, "    {} {spec}", if *enabled { "enable" } else { "disable" })?;
        }
    }
    if !any {
        writeln!(out, "  (none)")?;
    }

    writeln!(out, "\n{}", "artifacts differing from the catalog default".bold())?;
    let mut changed = false;
    for ecosystem in CATALOG {
        for (offset, artifact) in ecosystem.artifacts.iter().enumerate() {
            let spec = format!("{}.{}", ecosystem.id, artifact.slug());
            let Some(id) = crate::classify::ArtifactId::from_spec(&spec) else {
                continue;
            };
            let _ = offset;
            let enabled = merged.selection().is_enabled(id);
            if enabled != artifact.default_on {
                changed = true;
                writeln!(out, "  {} {spec}", if enabled { "on " } else { "off" })?;
            }
        }
    }
    if !changed {
        writeln!(out, "  (none)")?;
    }
    Ok(())
}

fn write_keep(out: &mut dyn io::Write, indent: &str, keep: KeepPolicy) -> io::Result<()> {
    if keep.is_empty() {
        return writeln!(out, "{indent}(unset)");
    }
    if let Some(min_age) = keep.min_age {
        writeln!(out, "{indent}min_age  = {min_age:?}")?;
    }
    if let Some(min_size) = keep.min_size {
        writeln!(out, "{indent}min_size = {min_size}")?;
    }
    if let Some(max_size) = keep.max_size {
        writeln!(out, "{indent}max_size = {max_size}")?;
    }
    Ok(())
}
