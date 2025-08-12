use clap::Parser as _;
use embedded_storage_file::{NorMemoryAsync, NorMemoryInFile};
use iris as lib;
use tokio::io::AsyncBufReadExt;

/// Reads key value pairs from stdin and convertss them to a flashable settings partition binary
#[derive(clap::Parser)]
struct Args {
    /// Size of partition in bytes
    #[arg(short, long, default_value_t = 0x2000)]
    size: u32,
    /// Print written key and values on stdout
    #[arg(short, long)]
    verbose: bool,
    /// Output partition binary path
    bin_path: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let Args {
        size,
        bin_path,
        verbose,
    } = Args::parse();
    let range = 0..size;

    // READ_SIZE, WRITE_SIZE and ERASE_SIZE carefully chosen to be
    // same as esp_hal::FlashStorage's implementation
    let nor = NorMemoryInFile::<4, 4, 4096>::new(bin_path, size as usize)?;
    let storage = NorMemoryAsync::new(nor);

    let mut buf = [0u8; 512];
    let settings = lib::settings::Settings::uninit(storage, range, &mut buf);
    let mut settings = settings
        .reset()
        .await
        .map_err(|e| anyhow::anyhow!("settings reset failed: {e:?}"))?;

    let stdin = tokio::io::stdin();
    let reader = tokio::io::BufReader::new(stdin);
    let mut lines = reader.lines();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }

        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            if verbose {
                println!("{key}:{value}");
            }

            settings
                .set(key, &value.as_bytes())
                .await
                .map_err(|e| anyhow::anyhow!("setting set failed: {e:?}"))?;
        }
    }

    Ok(())
}
