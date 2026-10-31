//! `voom reap`: freeing the trash in the foreground, and the background reaper a run starts.

use clap::Args;

/// `voom reap` arguments.
#[derive(Debug, Args)]
pub struct ReapArgs {
    /// Run as the background reaper a sweep starts: lower priority, no output.
    #[arg(long, hide = true)]
    pub background: bool,

    /// List what is waiting in the trash, with sizes, without freeing it.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
}

impl ReapArgs {
    /// Frees the trash, reporting what it and earlier background reapers freed.
    ///
    /// # Errors
    ///
    /// When there is no trash location, or it cannot be opened, or the report cannot be written,
    /// or an entry survived the retry and is still in the trash.
    pub fn run(&self, out: &mut impl std::io::Write) -> anyhow::Result<()> {
        use anyhow::Context as _;
        let dir = crate::delete::trash_location().context("this platform has no cache directory for the trash")?;
        if self.background {
            crate::delete::yield_to_foreground();
            crate::delete::reap(&dir).context("freeing the trash")?;
            return Ok(());
        }
        if self.dry_run {
            return list_pending(&dir, out);
        }
        let progress = crate::run::removal_progress(true);
        let reaped = crate::delete::reap(&dir).context("freeing the trash");
        if let Some(progress) = progress {
            progress.finish_and_clear();
        }
        reaped?;
        let freed = crate::delete::collect_ledger(&dir);
        write_summary(&freed, &dir, out)?;
        if freed.failed_entries > 0 {
            anyhow::bail!(
                "{} trash entries could not be freed and remain in {}",
                freed.failed_entries,
                dir.display()
            );
        }
        Ok(())
    }
}

/// Lists the entries waiting in the trash, so a large trash is visible without freeing it.
fn list_pending(dir: &std::path::Path, out: &mut impl std::io::Write) -> anyhow::Result<()> {
    let trash = crate::delete::Trash::at(dir).map_err(|error| anyhow::anyhow!("opening {}: {error}", dir.display()))?;
    let pending = trash.pending();
    if pending.is_empty() {
        writeln!(out, "voom: the trash is empty")?;
        return Ok(());
    }
    let measured = crate::size::measure_all(&pending);
    let mut total = 0_u64;
    let mut unreadable = 0_usize;
    for (entry, size) in pending.iter().zip(&measured) {
        total += size.bytes;
        unreadable += size.unreadable;
        let name = entry
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("<unnamed>"))
            .to_string_lossy();
        writeln!(
            out,
            "  {:>10}  {name}",
            humansize::format_size(size.bytes, humansize::DECIMAL)
        )?;
    }
    writeln!(
        out,
        "voom: {} in {} entries in the trash; `voom reap` frees it",
        humansize::format_size(total, humansize::DECIMAL),
        pending.len()
    )?;
    if unreadable > 0 {
        writeln!(
            out,
            "voom: {unreadable} directories could not be read; the figure is a lower bound"
        )?;
    }
    Ok(())
}

/// Says what reapers freed, and what they could not.
///
/// # Errors
///
/// When the report cannot be written.
pub fn write_summary(
    freed: &crate::delete::Reaped,
    dir: &std::path::Path,
    out: &mut impl std::io::Write,
) -> std::io::Result<()> {
    if freed.freed_entries == 0 && freed.bytes == 0 && freed.failed_entries == 0 {
        return writeln!(out, "voom: the trash is empty");
    }
    writeln!(
        out,
        "voom: the trash reaper freed {} from {} detached {}",
        humansize::format_size(freed.bytes, humansize::DECIMAL),
        freed.freed_entries,
        if freed.freed_entries == 1 {
            "artifact"
        } else {
            "artifacts"
        },
    )?;
    if freed.failed_entries > 0 {
        writeln!(
            out,
            "voom: {} could not be freed in full and {} still in {} — `voom reap` retries",
            freed.failed_entries,
            if freed.failed_entries == 1 { "is" } else { "are" },
            dir.display()
        )?;
    }
    Ok(())
}
