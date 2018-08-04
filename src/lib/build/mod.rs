//! Actually building Idris packages.

pub mod context;
pub mod invoke;
pub mod job;

use self::{context::BuildContext, invoke::CodegenInvocation, invoke::CompileInvocation};
use retrieve::cache::{Binary, OutputLayout, Source};
use std::{fs, path::PathBuf};
use util::{clear_dir, errors::Res};

/// A type of Target that should be built
#[derive(Clone, Copy, PartialOrd, Ord, PartialEq, Debug, Eq, Hash)]
pub enum Target {
    /// Typecheck a library without codegen
    Lib,
    /// Compile a standalone executable which doesn't require the package's lib to be
    /// built
    ///
    /// The usize field is the index of the BinTarget in the manifest's list of BinTargets which
    /// should be built
    Bin(usize),
    // Test is like Bin, except that it requires the lib to be built already.
    Test(usize),
    // I would assume creating documentation requires the lib to be built too
    /// Create documentation
    Doc,
}

#[derive(Clone, PartialEq, Debug, Eq, Hash)]
pub struct Targets(pub Vec<Target>);

impl Targets {
    pub fn new(mut ts: Vec<Target>) -> Self {
        ts.sort();

        let mut res = vec![];

        let mut seen_lib = false;

        for i in ts {
            match i {
                Target::Lib => {
                    if !seen_lib {
                        res.push(i);
                        seen_lib = true;
                    }
                }
                Target::Bin(_) => {
                    res.push(i);
                }
                Target::Test(_) => {
                    if !seen_lib {
                        seen_lib = true;
                        res.insert(0, Target::Lib);
                        res.push(i);
                    }
                }
                Target::Doc => {
                    if !seen_lib {
                        seen_lib = true;
                        res.insert(0, Target::Lib);
                        res.push(i);
                    }
                }
            }
        }

        Targets(res)
    }
}

pub fn compile_lib(
    source: &Source,
    deps: &[&Binary],
    layout: &OutputLayout,
    bcx: &BuildContext,
) -> Res<()> {
    let lib_target = source.meta().targets.lib.clone().ok_or_else(|| {
        format_err!(
            "package {} doesn't contain a lib target",
            source.meta().package.name
        )
    })?;

    clear_dir(&layout.lib)?;

    // We know that lib_target.path will be relative to the package root
    let src_path = source.path().join(&lib_target.path.0);
    let targets = lib_target
        .mods
        .iter()
        .map(|mod_name| {
            lib_target
                .path
                .0
                .join(mod_name.replace(".", "/"))
                .with_extension("idr")
        }).collect::<Vec<_>>();

    let invocation = CompileInvocation {
        pkg: source.meta().package.name.as_str(),
        src: &src_path,
        deps,
        targets: &targets,
        build: &layout.build.join("lib"),
    };

    invocation.exec(bcx)?;

    for target in targets {
        let target_bin = target.with_extension("ibc");
        let from = layout.build.join(&target_bin);
        // We strip the library prefix before copying
        // target_bin is something like src/Test.ibc
        // we want to move build/src/Test.ibc to lib/Test.ibc
        let to = layout
            .lib
            .join(&target_bin.strip_prefix(&src_path).unwrap());

        fs::create_dir_all(to.parent().unwrap())?;
        fs::rename(from, to)?;
    }

    Ok(())
}

// TODO: Return compilation result(path, meta or anything else)
pub fn compile_bin(
    source: &Source,
    target: Target,
    deps: &[&Binary],
    layout: &OutputLayout,
    bcx: &BuildContext,
) -> Res<()> {
    let bin_target = match target {
        Target::Bin(ix) => source.meta().targets.bin[ix].clone(),
        Target::Test(ix) => source.meta().targets.test[ix].clone(),
        _ => bail!("compile_bin called with non-binary target"),
    };

    // This is the full target path
    let target_path = source.path().join(bin_target.main.0).with_extension("idr");
    // TODO: Check this in manifest?
    let src_path = target_path.parent().unwrap();
    // This is the relative target path
    let target_path: PathBuf = target_path.file_name().unwrap().to_os_string().into();

    let compile_invoke = CompileInvocation {
        pkg: source.meta().package.name.as_str(),
        src: &src_path,
        deps,
        targets: &[target_path.clone()],
        build: &layout.build.join("bin"),
    };

    compile_invoke.exec(bcx)?;

    let target_bin = target_path.with_extension("ibc");

    let codegen_invoke = CodegenInvocation {
        pkg: source.meta().package.name.as_str(),
        binary: &layout.build.join("bin").join(&target_bin),
        output: bin_target.name.clone(),
        // TODO
        backend: "c".to_string(),
        layout: &layout,
        is_artifact: false,
    };

    // The output exectable will always go in target/bin
    codegen_invoke.exec(bcx)?;

    Ok(())
}
