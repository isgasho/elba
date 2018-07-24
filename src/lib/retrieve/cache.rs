//! Caching packages which have been downloaded before.
//!
//! ## Background: Previous design
//! Previous designs for `elba` indices alluded to a feature where the local package cache would
//! be formatted as an index itself, with its entries pointing to locations on disk where
//! downloaded packages (whether as a result of DirectRes deps by root or through Indices)
//! are located. However, this relied on the concept of "index overlapping" in which, in the case
//! of multiple indices having the same name, the package from the "higher priority" index would be
//! picked. In this previous design, the package from the "cache index" would be picked, avoiding
//! re-downloading of packages.
//!
//! However, this design of overlapping indices was abandoned because it made package resolution
//! unreliable and dependent on global state. Additionally, because an Index can only store and
//! manage metadata information, a separate Cache struct would've been needed anyway to manage
//! caching, making this design complex and complicated.
//!
//! ## Current design
//! In this new design, there is a much clearer separation between storing package metadata from
//! remote sources (handled strictly by Indices) and caching the packages themselves locally
//! (which is handled by the Cache struct). The Cache struct is responsible for determining if a
//! package has already been retrieved from the Internet, and coordinates cached package downloads.
//!
//! The Cache struct must be responsible for a directory which contains previously
//! downloaded packages from all sources, and should deal with checksums and things like that to see if
//! a redownload is needed. Whenever a package is about to be downloaded, the Cache is there to see
//! if it really actually needs to be downloaded.
//!
//! The Cache doesn't need its own Index; the point of an Index is to cache metadata about packages,
//! but the Cache already has fully downloaded packages with manifests included, so it can just
//! peek at the manifests to find out about package information. Every package will get a directory
//! according to its summary, which is how the Cache will know what packages are available. Git repos
//! should be cloned into the path of the Cache, and local dir dependencies should be symlinked in.
//!
//! ### Future potential
//! This new design for the cache makes possible several desirable features which could easily be
//! implemented in the future.
//!
//! #### "Airplane mode"
//! If a user does not want to access the Internet to resolve packages, `elba` can limit itself
//! to only using the packages provided by the Cache.
//!
//! #### Vendoring
//! In order to vendor packages, `elba` can create a new Cache in the project directory and require
//! that all packages originate from the vendor directory (basically airplane mode + custom cache
//! directory). Directory dependencies should be copied into the Cache directory unconditionally.
//! From there, the user should change their manifest so that it points to the vendored directory.
//!
//! #### Build caching
//! If we want to cache builds, we can just have a separate subfolder for ibcs.

use failure::{Error, ResultExt};
use index::{Index, Indices};
use package::{
    manifest::Manifest,
    resolution::DirectRes,
    Name, PackageId,
};
use petgraph::visit::{Bfs, IntoNodeReferences, Walker};
use reqwest::Client;
use resolve::solve::SourceSolve;
use semver::Version;
use sha2::{Digest, Sha256};
use slog::Logger;
use std::{
    collections::VecDeque,
    fs,
    io::{prelude::*, BufReader},
    path::PathBuf,
    str::FromStr,
};
use tar::Builder;
use util::{
    copy,
    errors::{ErrorKind, Res},
    hexify_hash,
    lock::DirLock,
};

/// A Cache of downloaded packages and packages with no other Index.
///
/// A Cache is located in a directory, and it has two directories of its own:
/// - `src/`: the cache of downloaded packages, in full source form.
/// - `build/`: the cache of built packages.
///
/// The src and build folders contain one folder for every package on disk.
// TODO: Maybe the Cache is in charge of the Indices. This way, metadata takes into account both
// indices and direct deps, and we don't have to discriminate between the two in the Retriever.
#[derive(Debug, Clone)]
pub struct Cache {
    location: PathBuf,
    client: Client,
    pub logger: Logger,
}

impl Cache {
    pub fn from_disk(plog: &Logger, location: PathBuf) -> Self {
        let _ = fs::create_dir_all(location.join("src"));
        let _ = fs::create_dir_all(location.join("build"));
        let _ = fs::create_dir_all(location.join("indices"));

        let client = Client::new();
        let logger = plog.new(o!("location" => location.to_string_lossy().into_owned()));

        Cache {
            location,
            client,
            logger,
        }
    }

    /// Retrieve the metadata of a package, loading it into the cache if necessary.
    pub fn checkout_source(
        &self,
        pkg: &PackageId,
        loc: &DirectRes,
        v: Option<&Version>,
    ) -> Result<Source, Error> {
        let p = self.load_source(pkg, loc, v)?;
        let mf_path = p.path().join("elba.toml");

        let file = fs::File::open(mf_path).context(ErrorKind::MissingManifest)?;
        let mut file = BufReader::new(file);
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .context(ErrorKind::InvalidIndex)?;

        let manifest = Manifest::from_str(&contents).context(ErrorKind::InvalidIndex)?;

        if manifest.summary().name() != pkg.name() {
            return Err(format_err!(
                "names don't match: {} was declared, but {} was found in elba.toml",
                pkg.name(),
                manifest.summary().name()
            ))?;
        }

        Source::from_folder(manifest, loc.clone(), p)
    }

    // TODO: In the future (heh), return Box<Future<Item = PathBuf, Error = Error>> and use async
    // reqwest. For now, it seems like too much trouble for not that much gain.
    // Info on async:
    // https://stackoverflow.com/questions/49087958/getting-multiple-urls-concurrently-with-hyper
    // Info on downloading things in general:
    // https://rust-lang-nursery.github.io/rust-cookbook/web/clients/download.html
    /// Returns a future pointing to the path to a downloaded (and potentially extracted, if it's a
    /// tarball) package.
    ///
    /// If the package has been cached, this function does no I/O. If it hasn't, it goes wherever
    /// it needs to in order to retrieve the package.
    fn load_source(
        &self,
        pkg: &PackageId,
        loc: &DirectRes,
        v: Option<&Version>,
    ) -> Result<DirLock, Error> {
        if let Some(path) = self.check_source(pkg.name(), loc, v) {
            DirLock::acquire(&path)
        } else {
            let mut p = self.location.clone();
            p.push("src");
            p.push(Self::get_source_dir(pkg.name(), loc, v));

            let dir = DirLock::acquire(&p)?;
            loc.retrieve(&self.client, &dir)?;

            Ok(dir)
        }
    }

    // TODO: Workspaces for git repos.
    /// Check if package is downloaded and in the cache. If so, returns the path of source of the cached
    /// package.
    fn check_source(&self, name: &Name, loc: &DirectRes, v: Option<&Version>) -> Option<PathBuf> {
        if let DirectRes::Dir { url } = loc {
            return Some(url.clone());
        }

        let mut path = self.location.clone();
        path.push("src");
        path.push(Self::get_source_dir(name, loc, v));
        if path.exists() {
            Some(path)
        } else {
            None
        }
    }

    /// Gets the corresponding directory of a package. We need this because for packages which have
    /// no associated version (i.e. git and local dependencies, where the constraints are inherent
    /// in the resolution itself), we ignore a version specifier.
    ///
    /// Note: with regard to git repos, we treat the same repo with different checked out commits/
    /// tags as completely different repos.
    fn get_source_dir(name: &Name, loc: &DirectRes, v: Option<&Version>) -> String {
        let mut hasher = Sha256::default();
        hasher.input(name.as_bytes());
        hasher.input(loc.to_string().as_bytes());
        if let Some(v) = v {
            // We only care about the version of the package at this source directory if it came
            // from a tarball
            if let DirectRes::Tar {
                url: _url,
                cksum: _cksum,
            } = loc
            {
                hasher.input(v.to_string().as_bytes());
            }
        }
        let hash = hexify_hash(hasher.result().as_slice());

        format!("{}_{}-{}", name.group(), name.name(), hash)
    }

    pub fn checkout_build(&self, root: &Source, graph: &SourceSolve) -> Result<Binary, Error> {
        let binary_path = self.load_build(root, graph)?;
        Ok(Binary { binary_path })
    }

    /// Acquires a lock on a build directory, copying the files from the source if needed.
    fn load_build(&self, root: &Source, graph: &SourceSolve) -> Result<DirLock, Error> {
        if let Some(path) = self.check_build(root, graph) {
            DirLock::acquire(&path)
        } else {
            let p = self
                .location
                .join("build")
                .join(Self::get_build_dir(root, graph));

            let dir = DirLock::acquire(&p)?;

            copy(root.path.path(), dir.path())?;

            Ok(dir)
        }
    }

    fn check_build(&self, root: &Source, graph: &SourceSolve) -> Option<PathBuf> {
        let path = self
            .location
            .join("build")
            .join(Self::get_build_dir(root, graph));

        if path.exists() {
            Some(path)
        } else {
            None
        }
    }

    fn get_build_dir(root: &Source, graph: &SourceSolve) -> String {
        let mut hasher = Sha256::default();

        let rix = graph
            .node_references()
            .find(|(_, s)| root.hash == s.hash)
            .map(|(ix, _)| ix)
            .unwrap();

        let search = Bfs::new(graph, rix).iter(graph);

        for pkg in search {
            let src = &graph[pkg];
            hasher.input(&src.hash.as_bytes());
        }

        hexify_hash(hasher.result().as_slice())
    }

    fn get_index_dir(loc: &DirectRes) -> String {
        let mut hasher = Sha256::default();
        hasher.input(loc.to_string().as_bytes());
        hexify_hash(hasher.result().as_slice())
    }

    // TODO: We do a lot of silent erroring. Is that good?
    pub fn get_indices(&self, index_reses: &[DirectRes]) -> Indices {
        let mut indices = vec![];
        let mut q: VecDeque<DirectRes> = index_reses.iter().cloned().collect();

        while let Some(index) = q.pop_front() {
            // We special-case a local dir index because `dir` won't exist for it.
            if let DirectRes::Dir { url } = &index {
                let lock = if let Ok(dir) = DirLock::acquire(url) {
                    dir
                } else {
                    continue;
                };

                let ix = Index::from_disk(index.clone(), lock);
                if let Ok(ix) = ix {
                    for dependent in ix.depends().iter().cloned().map(|i| i.res) {
                        q.push_back(dependent);
                    }
                    indices.push(ix);
                }
                continue;
            }

            let dir = if let Ok(dir) = DirLock::acquire(
                &self
                    .location
                    .join("indices")
                    .join(Self::get_index_dir(&index)),
            ) {
                dir
            } else {
                continue;
            };

            if dir.path().exists() {
                let ix = Index::from_disk(index.clone(), dir);
                if let Ok(ix) = ix {
                    for dependent in ix.depends().iter().cloned().map(|i| i.res) {
                        q.push_back(dependent);
                    }
                    indices.push(ix);
                }
                continue;
            }

            if index.retrieve(&self.client, &dir).is_ok() {
                let ix = Index::from_disk(index.clone(), dir);
                if let Ok(ix) = ix {
                    for dependent in ix.depends().iter().cloned().map(|i| i.res) {
                        q.push_back(dependent);
                    }
                    indices.push(ix);
                }
            }
        }

        Indices::new(indices)
    }
}

/// Information about the source of package that is available somewhere in the file system.
/// Packages are stored as directories on disk (not archives because it would just be a bunch of
/// pointless unpacking-repacking).
#[derive(Debug)]
pub struct Source {
    /// The package's manifest
    pub meta: Manifest,
    pub location: DirectRes,
    /// The path to the package.
    pub path: DirLock,
    pub hash: String,
}

impl Source {
    /// The purpose of having a hash is for builds. The resolution graph only stores Summaries. If we were
    /// to rely solely on hashing the Summaries of a package's dependencies to determine if we need
    /// to rebuild a package, we'd run into a big problem: a package would only get rebuilt iff its
    /// own version changed or a version of one of its dependents changed. This is a problem for
    /// DirectRes deps, since they can change often without changing their version, leading to
    /// erroneous cases where packages aren't rebuilt. Even if we were to use the hash that
    /// determines the folder name of a package, it wouldn't be enough. Local dependencies' folder
    /// names never change and don't have a hash, and git repos which pin themselves to a branch
    /// can maintain the same hash while updating their contents.
    ///
    /// Not only that, but stray ibc files and other miscellaneous crap could get into the source
    /// directory, which would force us to rebuild yet again.
    ///
    /// To remedy this, we'd like to have a hash that indicates that the file contents of a Source
    /// have changed.
    ///
    /// Note that the hash stored differs from the hash used to determine if a package needs to be
    /// redownloaded completely; for git repos, if the resolution is to use master, then the same
    /// folder will be used, but will be checked out to the latest master every time.
    pub fn from_folder(meta: Manifest, location: DirectRes, path: DirLock) -> Res<Self> {
        // Pack into a tar file to hash it quickly
        let f = fs::File::create(path.path().with_extension("tar"))?;
        let mut ar = Builder::new(f);
        ar.append_dir_all("irrelevant", path.path())?;

        let _ = ar.into_inner()?;

        let p = path.path().with_extension("tar");
        let mut file = fs::File::open(&p)?;
        let result = Sha256::digest_reader(&mut file)?;
        let hash = hexify_hash(result.as_slice());

        // The tarball is useless to us now.
        drop(file);
        fs::remove_file(p)?;

        Ok(Source {
            meta,
            location,
            path,
            hash,
        })
    }
}

/// Information about the build of library that is available somewhere in the file system.
#[derive(Debug)]
pub struct Binary {
    /// The path to ibc binary
    binary_path: DirLock,
}
