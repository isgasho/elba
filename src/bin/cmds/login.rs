use super::{args, get};
use clap::{App, Arg, ArgMatches, SubCommand};
use elba::{
    cli::backend,
    util::{config::Config, errors::Res},
};

pub fn cli() -> App<'static, 'static> {
    SubCommand::with_name("login")
        .about("Log in to a given backend")
        .arg(Arg::with_name("token").takes_value(true).required(true))
        .arg(args::index())
}

pub fn exec(c: &mut Config, args: &ArgMatches) -> Res<String> {
    let bck = get::index(c, args)?;
    let ctx = backend::BackendCtx {
        index: bck,
        data_dir: c.directories.data.clone(),
    };
    let token = args.value_of("token").unwrap();

    backend::login(&ctx, &token)
}
