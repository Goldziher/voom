//! `voom reap`: freeing the trash in the foreground, and the background reaper a run starts.

use clap::Args;

/// `voom reap` arguments.
#[derive(Debug, Args)]
pub struct ReapArgs {
    /// Run as the background reaper a sweep starts: lower priority, no output.
    #[arg(long, hide = true)]
    pub background: bool,
}

impl ReapArgs {
    /// Frees the trash, reporting what it and earlier background reapers freed.
    ///
    /// # Errors
    ///
    /// When there is no trash location, or it cannot be opened, or the report cannot be written.
    pub fn run(&self, out: &mut impl std::io::Write) -> anyhow::Result<()> {
        use anyhow::Context as _;
        let dir = crate::delete::trash_location().context("this platform has no cache directory for the trash")?;
        if self.background {
            crate::delete::yield_to_foreground();
            crate::delete::reap(&dir).context("freeing the trash")?;
            return Ok(());
        }
        let progress = crate::run::removal_progress(true);
        let reaped = crate::delete::reap(&dir).context("freeing the trash");
        if let Some(progress) = progress {
            progress.finish_and_clear();
        }
        reaped?;
        let freed = crate::delete::collect_ledger(&dir);
        write_summary(&freed, &dir, out)?;
        Ok(())
    }
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
