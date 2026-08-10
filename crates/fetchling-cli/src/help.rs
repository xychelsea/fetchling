use crate::options::{OptMeta, Section, OPTIONS};

const DESCRIPTION: &str = "a modular, non-interactive, async network retriever written in Rust.";

pub fn print_version_short() {
    println!("fetchling {}", env!("CARGO_PKG_VERSION"));
}

pub fn print_version() {
    print_version_short();
    println!("{DESCRIPTION}");
    println!("MIT/Apache-2.0");
}

pub fn print_help() {
    println!("fetchling {}", env!("CARGO_PKG_VERSION"));
    println!("{DESCRIPTION}");
    println!();
    println!("Usage: fetchling [OPTION]... [URL]...");
    println!();
    println!("Examples:");
    println!("  fetchling https://example.com/file.bin");
    println!("  fetchling -O - https://example.com/robots.txt");
    println!("  fetchling -r -l 1 https://example.com/");
    println!();
    println!("-n short packs: -nc -nd -nH -np -nv");
    println!();

    let sections = [
        Section::Startup,
        Section::Logging,
        Section::Download,
        Section::Directory,
        Section::Http,
        Section::Https,
        Section::Warc,
        Section::Ftp,
        Section::Ftps,
        Section::Recursive,
        Section::AcceptReject,
    ];

    for section in sections {
        let opts: Vec<&OptMeta> = OPTIONS.iter().filter(|o| o.section == section).collect();
        if opts.is_empty() {
            continue;
        }
        let width = opts.iter().map(|o| format_flag(o).len()).max().unwrap_or(0);
        println!("{}", section.title());
        for opt in opts {
            let flag = format_flag(opt);
            println!("  {flag:<width$}  {}", opt.help);
        }
        println!();
    }
}

fn format_flag(opt: &OptMeta) -> String {
    let val = if opt.takes_value { "=ARG" } else { "" };
    match opt.short {
        Some(c) => format!("-{c}, --{}{val}", opt.long),
        None => format!("    --{}{val}", opt.long),
    }
}
