// TODO: remove this once we start actually developing the CLI tool
#![allow(unused_variables, dead_code)]

extern crate elba;
extern crate failure;
#[macro_use]
extern crate structopt;
extern crate toml;

use structopt::StructOpt;

// TODO: Custom tasks. elba-test is executed with elba t test.
// Interaction with the main repo would just be implemented as a custom task.
// Maybe tasks should be allowed to be designated in the manifest too.

#[derive(StructOpt)]
#[structopt(name = "elba", about = "An Idris package manager")]
struct Elba {
    #[structopt(short = "v", long = "verbose", parse(from_occurrences))]
    verbose: u8,
    #[structopt(subcommand)]
    cmd: Cmd,
}

#[derive(StructOpt)]
enum Cmd {
    #[structopt(name = "build")]
    Build,
    #[structopt(name = "check")]
    Check,
    #[structopt(name = "init")]
    Init {
        #[structopt(long = "lib")]
        lib: bool,
        name: String,
    },
    #[structopt(name = "index")]
    Index(IndexCmd),
    #[structopt(name = "install")]
    Install { specifier: String },
    #[structopt(name = "uninstall")]
    Uninstall { name: String },
    #[structopt(name = "repl")]
    Repl,
}

#[derive(StructOpt)]
enum IndexCmd {
    #[structopt(name = "new")]
    New,
}

fn main() {
    let opt = Elba::from_args();
}
