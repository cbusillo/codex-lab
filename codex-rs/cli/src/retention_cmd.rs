use clap::Parser;
use std::io::Write;

use codex_config::LoaderOverrides;
use codex_core::DEFAULT_RETENTION_PREVIEW_LIMIT;
use codex_core::MAX_RETENTION_PREVIEW_LIMIT;
use codex_core::RetentionPreviewPage;
use codex_core::RetentionPreviewParams;
use codex_core::config::ConfigBuilder;
use codex_utils_cli::CliConfigOverrides;

#[derive(Debug, Parser)]
pub(crate) struct RetentionCommand {
    #[command(subcommand)]
    subcommand: RetentionSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum RetentionSubcommand {
    /// Report retention candidates and protected rollouts without modifying storage.
    Preview(RetentionPreviewCommand),
}

#[derive(Debug, Parser)]
struct RetentionPreviewCommand {
    /// Maximum number of active and archived rollouts to return.
    #[arg(long, default_value_t = DEFAULT_RETENTION_PREVIEW_LIMIT, value_parser = parse_limit)]
    limit: usize,

    /// Opaque cursor returned by an earlier preview page.
    #[arg(long)]
    cursor: Option<String>,

    /// Render the machine-readable report as JSON.
    #[arg(long)]
    json: bool,
}

pub(crate) async fn run(
    command: RetentionCommand,
    config_overrides: CliConfigOverrides,
    loader_overrides: LoaderOverrides,
    strict_config: bool,
) -> anyhow::Result<()> {
    let cli_overrides = config_overrides
        .parse_overrides()
        .map_err(anyhow::Error::msg)?;
    let config = ConfigBuilder::default()
        .cli_overrides(cli_overrides)
        .loader_overrides(loader_overrides)
        .strict_config(strict_config)
        .build()
        .await?;
    let store = codex_core::read_only_thread_store_from_config(&config, /*state_db*/ None);

    match command.subcommand {
        RetentionSubcommand::Preview(preview) => {
            let report = store
                .retention_preview(RetentionPreviewParams {
                    limit: Some(preview.limit),
                    cursor: preview.cursor,
                })
                .await?;
            if preview.json {
                let stdout = std::io::stdout();
                let mut output = stdout.lock();
                serde_json::to_writer_pretty(&mut output, &report)?;
                writeln!(output)?;
            } else {
                print_text_report(&report);
            }
        }
    }
    Ok(())
}

fn print_text_report(report: &RetentionPreviewPage) {
    println!("Rollout retention preview (read-only)");
    println!(
        "Schema {} · {} item(s) · {} candidate(s) · {} protected",
        report.schema_version,
        report.items.len(),
        report.page_totals.candidate_count,
        report.page_totals.protected_count
    );
    println!();

    for item in &report.items {
        println!(
            "{}  {}  {}  {}",
            item.disposition.as_str(),
            item.collection.as_str(),
            item.thread_id,
            item.reason.as_str()
        );
        println!("  current: {}", format_bytes(item.current_storage_bytes));
        println!(
            "  estimated recoverable: {}",
            format_bytes(item.estimated_recoverable_bytes)
        );
        if let Some(path) = item.proposed_path.as_deref() {
            println!(
                "  proposed: {} -> {}",
                item.proposed_action.as_str(),
                path.display()
            );
        } else {
            println!("  proposed: {}", item.proposed_action.as_str());
        }
        if let Some(path) = item.recovery_path.as_deref() {
            println!("  recovery: {}", path.display());
        }
    }

    if report.items.is_empty() {
        println!("No rollouts were found on this page.");
    }
    println!();
    println!(
        "Page totals: {} current, {} estimated recoverable",
        format_bytes(report.page_totals.current_storage_bytes),
        format_bytes(report.page_totals.estimated_recoverable_bytes)
    );
    println!(
        "Metadata scan: {} rollout(s), {} unreadable, {} duplicate id(s), truncated={}",
        report.diagnostics.scanned_rollouts,
        report.diagnostics.unreadable_rollouts,
        report.diagnostics.duplicate_thread_ids,
        report.diagnostics.scan_truncated
    );
    if report.diagnostics.unreadable_rollouts > 0
        || report.diagnostics.duplicate_thread_ids > 0
        || report.diagnostics.scan_truncated
    {
        println!(
            "Classification suppressed: reference metadata is incomplete, so otherwise eligible rollouts remain protected."
        );
    }
    if let Some(cursor) = report.next_cursor.as_deref() {
        println!("Next cursor: {cursor}");
    }
    println!("Preview only: no files, locks, or database rows were changed.");
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.2} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn parse_limit(value: &str) -> Result<usize, String> {
    let limit = value.parse::<usize>().map_err(|_| {
        format!("limit must be an integer between 1 and {MAX_RETENTION_PREVIEW_LIMIT}")
    })?;
    if !(1..=MAX_RETENTION_PREVIEW_LIMIT).contains(&limit) {
        return Err(format!(
            "limit must be between 1 and {MAX_RETENTION_PREVIEW_LIMIT}"
        ));
    }
    Ok(limit)
}
