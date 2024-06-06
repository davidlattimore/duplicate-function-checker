use anyhow::bail;
use anyhow::Context;
use clap::Parser as _;
use object::Object as _;
use object::ObjectSection as _;
use object::ObjectSymbol;
use object::SectionKind;
use object::SymbolKind;
use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::Hash;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;

type Result<T = (), E = anyhow::Error> = core::result::Result<T, E>;

/// A tool to determine what percentage of a binary's functions are excess duplicates. A symbol
/// table is needed and functions in the symbol table need to have non-zero sizes.
#[derive(clap::Parser)]
struct Args {
    /// Input binary to parse.
    bin: PathBuf,

    /// Whether to print information about each duplicate symbol.
    #[arg(long)]
    verbose: bool,

    /// Whether to demangle symbol names.
    #[arg(long)]
    demangle: bool,

    /// Whether to demangle symbol names and drop rust's hashes.
    #[arg(long)]
    demangle_no_hash: bool,

    /// What to key functions by.
    #[arg(long, default_value = "instructions")]
    key: KeyType,

    /// What to sort results by.
    #[arg(long, default_value = "excess-bytes")]
    sort: SortType,
}

#[derive(Clone, Copy, clap::ValueEnum, PartialEq, Eq)]
enum KeyType {
    /// Group by normalised instruction bytes.
    Instructions,

    /// Key by function name and size.
    NameAndSize,

    /// Key by function name and size, but drop the hash added by rustc. This may group
    /// monomorphisations that are fundamentally different, so isn't recommended.
    NameWithoutRustHash,
}

#[derive(Clone, Copy, clap::ValueEnum, PartialEq, Eq)]
enum SortType {
    /// Sort by excess bytes of function in the binary.
    ExcessBytes,

    /// Sort by number of copies.
    Copies,

    /// Sort by function size.
    Size,
}

fn main() -> Result {
    let args = Args::parse();
    let r = match args.key {
        KeyType::NameAndSize => process::<NameAndSizeKey>(&args.bin, &args),
        KeyType::NameWithoutRustHash => process::<NameAndSizeKey>(&args.bin, &args),
        KeyType::Instructions => process::<InstructionsKey>(&args.bin, &args),
    };
    r.with_context(|| format!("Failed to process `{}`", args.bin.display()))?;
    Ok(())
}

trait Key: Hash + Eq + Sized {
    fn from_sym<'data>(
        sym: &object::Symbol<'data, '_, &'data [u8]>,
        inputs: &KeyBuilderInputs,
    ) -> Option<Self>;
}

fn process<K: Key>(path: &Path, args: &Args) -> Result {
    let data = std::fs::read(path)?;
    let object = object::File::parse(data.as_slice())?;
    let mut symbols = HashMap::new();

    let inputs = KeyBuilderInputs::new(&object, args);
    let mut considered = 0;

    for sym in object.symbols() {
        if sym.kind() != SymbolKind::Text || sym.size() == 0 {
            continue;
        }
        let Some(key) = K::from_sym(&sym, &inputs) else {
            continue;
        };
        considered += 1;
        let info = symbols.entry(key).or_insert_with(|| SymInfo {
            count: 0,
            names: Default::default(),
            function_size: sym.size(),
        });
        info.count += 1;
        if let Ok(name) = sym.name() {
            let key = if args.demangle {
                Cow::Owned(rustc_demangle::demangle(name).to_string())
            } else if args.demangle_no_hash {
                Cow::Owned(format!("{:#}", rustc_demangle::demangle(name)))
            } else {
                Cow::Borrowed(name)
            };
            *info.names.entry(key).or_default() += 1;
        };
    }

    let (duplicated_bytes, duplicated_functions, duplicate_instances) =
        symbols.values().fold((0, 0, 0), |prev, v| {
            (
                prev.0 + v.excess_bytes(),
                prev.1 + if v.count > 1 { 1 } else { 0 },
                prev.2 + v.count.saturating_sub(1),
            )
        });

    let text_size = determine_text_size(&object);
    let percent = duplicated_bytes as f64 / text_size as f64;

    if args.verbose {
        print_duplicates(symbols, args.sort)?;
    }

    if considered == 0 {
        if object.symbols().next().is_none() {
            bail!("Binary has no symbol table");
        }
        bail!("No functions were checked for duplication, symbols may have zero sizes");
    }

    println!(
        "Original binary: {} of executable code",
        pretty_size(text_size)
    );
    println!(
        "   Excess bytes: {} ({:.1}% of executable code)",
        pretty_size(duplicated_bytes),
        percent * 100.0
    );
    println!(
        "            Fns: {duplicated_functions} with dupes, {duplicate_instances} excess instances"
    );

    Ok(())
}

fn get_fn_bytes<'data>(
    sym: &object::Symbol<'data, '_, &'data [u8]>,
    object: &object::File<'data, &'data [u8]>,
) -> Option<&'data [u8]> {
    let section = object.section_by_index(sym.section_index()?).ok()?;
    let section_data = section.data().ok()?;
    let offset = sym.address().checked_sub(section.address())? as usize;
    let end = offset + sym.size() as usize;
    if end > section_data.len() {
        return None;
    }
    Some(&section_data[offset..end])
}

fn print_duplicates<K: Key>(symbols: HashMap<K, SymInfo>, sort: SortType) -> Result {
    let mut symbols = symbols
        .into_values()
        .filter(|info| info.count > 1)
        .collect::<Vec<_>>();

    match sort {
        SortType::ExcessBytes => symbols.sort_by_key(|v| v.excess_bytes()),
        SortType::Copies => symbols.sort_by_key(|v| v.count),
        SortType::Size => symbols.sort_by_key(|v| v.function_size),
    };

    let mut out = std::io::stdout().lock();
    for v in symbols {
        writeln!(&mut out, "Function size: {}", pretty_size(v.function_size))?;
        writeln!(&mut out, "Copies: {}", v.count)?;
        writeln!(&mut out, "Excess bytes: {}", pretty_size(v.excess_bytes()))?;
        writeln!(&mut out, "Names:")?;
        for (name, count) in &v.names {
            writeln!(&mut out, "  {count}x `{name}`")?;
        }
        writeln!(&mut out)?;
    }
    Ok(())
}

fn determine_text_size<'data>(object: &object::File<'data, &'data [u8]>) -> u64 {
    object
        .sections()
        .map(|sec| {
            if sec.kind() == SectionKind::Text {
                sec.size()
            } else {
                0
            }
        })
        .sum()
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct NameAndSizeKey {
    demangled_name: String,
    function_size: u64,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct InstructionsKey {
    function_bytes: Vec<u8>,
}

struct KeyBuilderInputs<'data, 'inputs> {
    max_fn_address: u64,
    object: &'inputs object::File<'data, &'data [u8]>,
    args: &'inputs Args,
}
impl<'data, 'inputs> KeyBuilderInputs<'data, 'inputs> {
    fn new(object: &'inputs object::File<'data, &'data [u8]>, args: &'inputs Args) -> Self {
        let max_fn_address = object
            .symbols()
            .filter(|s| s.kind() == SymbolKind::Text)
            .map(|s| s.address())
            .max()
            .unwrap_or(0);
        Self {
            max_fn_address,
            object,
            args,
        }
    }
}

impl Key for NameAndSizeKey {
    fn from_sym<'data>(
        sym: &object::Symbol<'data, '_, &'data [u8]>,
        inputs: &KeyBuilderInputs,
    ) -> Option<Self> {
        let Ok(name) = sym.name() else {
            return None;
        };
        let Ok(demangled) = rustc_demangle::try_demangle(name) else {
            return None;
        };
        let demangled_name = if inputs.args.key == KeyType::NameWithoutRustHash {
            format!("{demangled:#}")
        } else {
            demangled.to_string()
        };
        Some(NameAndSizeKey {
            demangled_name,
            function_size: sym.size(),
        })
    }
}

impl Key for InstructionsKey {
    fn from_sym<'data>(
        sym: &object::Symbol<'data, '_, &'data [u8]>,
        inputs: &KeyBuilderInputs,
    ) -> Option<Self> {
        let fn_bytes = get_fn_bytes(sym, inputs.object)?;
        // In order to determine if two functions at different addresses are the same, we need to
        // fix up IP-relative instructions. We relocate all our functions to the address of the last
        // function in the file. If we picked an earlier address, then some relative relocations
        // might wrap. If we chose a much later address, then we might exceed a 32 bit offset.
        // Although plausibly picking 2**31 would also work OK.
        let bytes = normalise_asm(fn_bytes, sym.address(), inputs.max_fn_address).ok()?;
        Some(Self {
            function_bytes: bytes,
        })
    }
}

struct SymInfo<'data> {
    count: u64,
    names: HashMap<Cow<'data, str>, u32>,
    function_size: u64,
}

impl SymInfo<'_> {
    fn excess_bytes(&self) -> u64 {
        self.count.saturating_sub(1) * self.function_size
    }
}

fn normalise_asm(fn_bytes: &[u8], base_address: u64, new_address: u64) -> Result<Vec<u8>> {
    const BIT_CLASS: u32 = 64;
    let options = iced_x86::DecoderOptions::NONE;
    let decoder = iced_x86::Decoder::with_ip(BIT_CLASS, fn_bytes, base_address, options);
    let instructions = decoder.into_iter().collect::<Vec<_>>();
    let block = iced_x86::InstructionBlock::new(&instructions, new_address);
    Ok(iced_x86::BlockEncoder::encode(64, block, iced_x86::BlockEncoderOptions::NONE)?.code_buffer)
}

fn pretty_size(size: u64) -> String {
    const KIBIBYTE: u64 = 1024;
    const MEBIBYTE: u64 = 1_048_576;
    const GIBIBYTE: u64 = 1_073_741_824;
    const TEBIBYTE: u64 = 1_099_511_627_776;
    const PEBIBYTE: u64 = 1_125_899_906_842_624;
    const EXBIBYTE: u64 = 1_152_921_504_606_846_976;

    let (size, symbol) = match size {
        size if size < KIBIBYTE => (size as f64, "B"),
        size if size < MEBIBYTE => (size as f64 / KIBIBYTE as f64, "KiB"),
        size if size < GIBIBYTE => (size as f64 / MEBIBYTE as f64, "MiB"),
        size if size < TEBIBYTE => (size as f64 / GIBIBYTE as f64, "GiB"),
        size if size < PEBIBYTE => (size as f64 / TEBIBYTE as f64, "TiB"),
        size if size < EXBIBYTE => (size as f64 / PEBIBYTE as f64, "PiB"),
        _ => (size as f64 / EXBIBYTE as f64, "EiB"),
    };

    format!("{:.1}{}", size, symbol)
}
