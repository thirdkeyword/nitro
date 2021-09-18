mod binary;
mod host;
mod machine;
mod memory;
mod merkle;
mod reinterpret;
mod utils;
mod value;
mod wavm;

use crate::{machine::Machine, wavm::Opcode, binary::WasmBinary};
use eyre::Result;
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    process,
};
use structopt::StructOpt;

#[derive(StructOpt)]
#[structopt(name = "arbitrator-prover")]
struct Opts {
    binary: PathBuf,
    #[structopt(short, long)]
    libraries: Vec<PathBuf>,
    #[structopt(short, long)]
    output: Option<PathBuf>,
    #[structopt(short = "b", long)]
    proving_backoff: bool,
    #[structopt(long)]
    always_merkleize: bool,
    #[structopt(short = "i", long, default_value = "1")]
    proving_interval: usize,
}

#[derive(Serialize)]
struct ProofInfo {
    before: String,
    proof: String,
    after: String,
}

fn parse_binary(path: &Path) -> Result<WasmBinary> {
    let mut f = File::open(path)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;

    let bin = match binary::parse(&buf) {
        Ok(bin) => bin,
        Err(err) => {
            eprintln!("Parsing error:");
            for (input, kind) in err.errors {
                eprintln!("Got {:?} while parsing {}", kind, hex::encode(&input[..64]));
            }
            process::exit(1);
        }
    };

    Ok(bin)
}

fn main() -> Result<()> {
    let opts = Opts::from_args();

    let mut libraries = Vec::new();
    for lib in &opts.libraries {
        libraries.push(parse_binary(lib)?);
    }
    let main_mod = parse_binary(&opts.binary)?;

    let out = opts.output.map(File::create).transpose()?;

    let mut proofs: Vec<ProofInfo> = Vec::new();
    let mut mach = Machine::from_binary(libraries, main_mod, opts.always_merkleize);
    println!("Starting machine hash: {}", mach.hash());

    let mut seen_states = HashSet::new();
    let mut opcode_counts: HashMap<Opcode, usize> = HashMap::new();
    while !mach.is_halted() {
        let next_inst = mach.get_next_instruction().unwrap();
        let next_opcode = next_inst.opcode;
        if opts.proving_backoff {
            let count_entry = opcode_counts.entry(next_opcode).or_insert(0);
            *count_entry += 1;
            let count = *count_entry;
            // Apply an exponential backoff to how often to prove an instruction;
            let prove = count < 10
                || (count < 100 && count % 10 == 0)
                || (count < 1000 && count % 100 == 0);
            if !prove {
                mach.step();
                continue;
            }
        }
        println!("Machine stack: {:?}", mach.get_data_stack());
        print!(
            "Generating proof \x1b[36m#{}\x1b[0m of opcode \x1b[32m{:?}\x1b[0m with data 0x{:x}",
            proofs.len(),
            next_opcode,
            next_inst.argument_data,
        );
        std::io::stdout().flush().unwrap();
        let before = mach.hash();
        if !seen_states.insert(before) {
            break;
        }
        let proof = mach.serialize_proof();
        mach.step();
        let after = mach.hash();
        println!(" - done");
        proofs.push(ProofInfo {
            before: before.to_string(),
            proof: hex::encode(proof),
            after: after.to_string(),
        });
        for _ in 1..opts.proving_interval {
            mach.step();
        }
    }

    println!("End machine hash: {}", mach.hash());
    println!("End machine stack: {:?}", mach.get_data_stack());
    println!("End machine backtrace:");
    for (module, func, pc) in mach.get_backtrace() {
        let func = rustc_demangle::demangle(&func);
        println!(
            "  {} \x1b[32m{}\x1b[0m @ \x1b[36m{}\x1b[0m",
            module, func, pc
        );
    }
    let output = mach.get_stdio_output();
    println!("End machine output:");
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    stdout
        .write_all(output)
        .expect("Failed to write machine output to stdout");

    if let Some(out) = out {
        serde_json::to_writer_pretty(out, &proofs)?;
    }

    Ok(())
}
