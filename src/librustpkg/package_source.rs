// Copyright 2012-2013 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern mod extra;

use target::*;
use std::io;
use std::io::fs;
use std::os;
use context::*;
use crate::Crate;
use messages::*;
use source_control::{safe_git_clone, git_clone_url, DirToUse, CheckedOutSources};
use source_control::make_read_only;
use path_util::{find_dir_using_rust_path_hack, make_dir_rwx_recursive, default_workspace};
use path_util::{target_build_dir, versionize, dir_has_crate_file};
use util::{compile_crate, DepMap};
use workcache_support;
use workcache_support::{digest_only_date, digest_file_with_date, crate_tag};
use extra::workcache;
use extra::treemap::TreeMap;
use syntax::crateid::CrateId;
use rustc::driver::session;

// An enumeration of the unpacked source of a package workspace.
// This contains a list of files found in the source workspace.
#[deriving(Clone)]
pub struct PkgSrc {
    /// Root of where the package source code lives
    source_workspace: Path,
    /// If build_in_destination is true, temporary results should
    /// go in the build/ subdirectory of the destination workspace.
    /// (Otherwise, they go in the build/ subdirectory of the
    /// source workspace.) This happens if the "RUST_PATH hack" is
    /// in effect, or if sources were fetched from a remote
    /// repository.
    build_in_destination: bool,
    /// Where to install the results. May or may not be the same
    /// as source_workspace
    destination_workspace: Path,
    // Directory to start looking in for packages -- normally
    // this is workspace/src/id but it may be just workspace
    start_dir: Path,
    id: CrateId,
    libs: ~[Crate],
    mains: ~[Crate],
    tests: ~[Crate],
    benchs: ~[Crate],
}

pub enum BuildSort { InPlace, Discovered }

impl ToStr for PkgSrc {
    fn to_str(&self) -> ~str {
        format!("Package ID {} in start dir {} [workspaces = {} -> {}]",
                self.id.to_str(),
                self.start_dir.display(),
                self.source_workspace.display(),
                self.destination_workspace.display())
    }
}
condition! {
    build_err: (~str) -> ~str;
}

fn prefixes(p: &Path) -> Prefixes {
    Prefixes {
        components: p.str_components().map(|x|x.unwrap().to_owned()).to_owned_vec(),
        remaining: ~[]
    }
}

struct Prefixes {
    priv components: ~[~str],
    priv remaining: ~[~str]
}

impl Iterator<(Path, Path)> for Prefixes {
    #[inline]
    fn next(&mut self) -> Option<(Path, Path)> {
        if self.components.len() <= 1 {
            None
        }
        else {
            let last = self.components.pop().unwrap();
            self.remaining.unshift(last);
            // converting to str and then back is a little unfortunate
            Some((Path::new(self.components.connect("/")),
                  Path::new(self.remaining.connect("/"))))
        }
    }
}

impl PkgSrc {
    pub fn new(mut source_workspace: Path,
               destination_workspace: Path,
               use_rust_path_hack: bool,
               id: CrateId) -> PkgSrc {
        use conditions::nonexistent_package::cond;

        debug!("Checking package source for package ID {}, \
                workspace = {} -> {}, use_rust_path_hack = {:?}",
                id.to_str(),
                source_workspace.display(),
                destination_workspace.display(),
                use_rust_path_hack);

        let mut destination_workspace = destination_workspace.clone();

        let mut to_try = ~[];
        let mut output_names = ~[];
        let build_dir = target_build_dir(&source_workspace);

        if use_rust_path_hack {
            to_try.push(source_workspace.clone());
        } else {
            // We search for sources under both src/ and build/ , because build/ is where
            // automatically-checked-out sources go.
            let path = Path::new(id.path.as_slice());
            let mut result = source_workspace.join("src");
            result.push(&path.dir_path());
            result.push(id.short_name_with_version());
            to_try.push(result);
            let mut result = source_workspace.join("src");
            result.push(&path);
            to_try.push(result);

            let mut result = build_dir.join("src");
            result.push(&path.dir_path());
            result.push(id.short_name_with_version());
            to_try.push(result.clone());
            output_names.push(result);
            let mut other_result = build_dir.join("src");
            other_result.push(&path);
            to_try.push(other_result.clone());
            output_names.push(other_result);

        }

        debug!("Checking dirs: {:?}", to_try.map(|p| p.display().to_str()).connect(":"));

        let path = to_try.iter().find(|&d| d.is_dir()
                                      && dir_has_crate_file(d));

        // See the comments on the definition of PkgSrc
        let mut build_in_destination = use_rust_path_hack;
        debug!("1. build_in_destination = {:?}", build_in_destination);

        let dir: Path = match path {
            Some(d) => (*d).clone(),
            None => {
                // See if any of the prefixes of this package ID form a valid package ID
                // That is, is this a package ID that points into the middle of a workspace?
                for (prefix, suffix) in prefixes(&Path::new(id.path.as_slice())) {
                    let crate_id: Option<CrateId> = from_str(prefix.as_str().unwrap());
                    let crate_id = crate_id.expect("valid crate id");
                    let path = build_dir.join(crate_id.path.as_slice());
                    debug!("in loop: checking if {} is a directory", path.display());
                    if path.is_dir() {
                        let ps = PkgSrc::new(source_workspace,
                                             destination_workspace,
                                             use_rust_path_hack,
                                             crate_id);
                        match ps {
                            PkgSrc {
                                source_workspace: source,
                                destination_workspace: destination,
                                start_dir: start,
                                id: id, .. } => {
                                let result = PkgSrc {
                                    source_workspace: source.clone(),
                                    build_in_destination: build_in_destination,
                                    destination_workspace: destination,
                                    start_dir: start.join(&suffix),
                                    id: id,
                                    libs: ~[],
                                    mains: ~[],
                                    tests: ~[],
                                    benchs: ~[]
                                };
                                debug!("pkgsrc: Returning {}", result.to_str());
                                return result;
                            }
                        }

                    }
                }

                // Ok, no prefixes work, so try fetching from git
                let mut ok_d = None;
                for w in output_names.iter() {
                    debug!("Calling fetch_git on {}", w.display());
                    let target_dir_opt = PkgSrc::fetch_git(w, &id);
                    for p in target_dir_opt.iter() {
                        ok_d = Some(p.clone());
                        build_in_destination = true;
                        debug!("2. build_in_destination = {:?}", build_in_destination);
                        break;
                    }
                    match ok_d {
                        Some(ref d) => {
                            let path = Path::new(id.path.as_slice());
                            if d.is_ancestor_of(&path)
                                || d.is_ancestor_of(&versionize(id.path, &id.version)) {
                                // Strip off the package ID
                                source_workspace = d.clone();
                                for _ in path.components() {
                                    source_workspace.pop();
                                }
                                // Strip off the src/ part
                                source_workspace.pop();
                                // Strip off the build/<target-triple> part to get the workspace
                                destination_workspace = source_workspace.clone();
                                destination_workspace.pop();
                                destination_workspace.pop();
                            }
                            break;
                        }
                        None => ()
                    }
                }
                match ok_d {
                    Some(d) => d,
                    None => {
                        // See if the sources are in $CWD
                        let cwd = os::getcwd();
                        if dir_has_crate_file(&cwd) {
                            return PkgSrc {
                                // In this case, source_workspace isn't really a workspace.
                                // This data structure needs yet more refactoring.
                                source_workspace: cwd.clone(),
                                destination_workspace: default_workspace(),
                                build_in_destination: true,
                                start_dir: cwd,
                                id: id,
                                libs: ~[],
                                mains: ~[],
                                benchs: ~[],
                                tests: ~[]
                            }
                        } else if use_rust_path_hack {
                            match find_dir_using_rust_path_hack(&id) {
                                Some(d) => d,
                                None => {
                                    cond.raise((id.clone(),
                                        ~"supplied path for package dir does not \
                                        exist, and couldn't interpret it as a URL fragment"))
                                }
                            }
                        } else {
                            cond.raise((id.clone(),
                                ~"supplied path for package dir does not \
                                exist, and couldn't interpret it as a URL fragment"))
                        }
                    }
                }
            }
        };
        debug!("3. build_in_destination = {:?}", build_in_destination);
        debug!("source: {} dest: {}", source_workspace.display(), destination_workspace.display());

        debug!("For package id {}, returning {}", id.to_str(), dir.display());

        if !dir.is_dir() {
            cond.raise((id.clone(), ~"supplied path for package dir is a \
                                        non-directory"));
        }

        PkgSrc {
            source_workspace: source_workspace.clone(),
            build_in_destination: build_in_destination,
            destination_workspace: destination_workspace,
            start_dir: dir,
            id: id,
            libs: ~[],
            mains: ~[],
            tests: ~[],
            benchs: ~[]
        }
    }

    /// Try interpreting self's package id as a git repository, and try
    /// fetching it and caching it in a local directory. Return the cached directory
    /// if this was successful, None otherwise. Similarly, if the package id
    /// refers to a git repo on the local version, also check it out.
    /// (right now we only support git)
    pub fn fetch_git(local: &Path, crateid: &CrateId) -> Option<Path> {
        use conditions::git_checkout_failed::cond;

        let cwd = os::getcwd();
        let path = Path::new(crateid.path.as_slice());
        debug!("Checking whether {} (path = {}) exists locally. Cwd = {}, does it? {:?}",
                crateid.to_str(), crateid.path,
                cwd.display(),
                path.exists());

        match safe_git_clone(&path, &crateid.version, local) {
            CheckedOutSources => {
                make_read_only(local);
                Some(local.clone())
            }
            DirToUse(clone_target) => {
                if path.components().nth(1).is_none() {
                    // If a non-URL, don't bother trying to fetch
                    return None;
                }

                // FIXME (#9639): This needs to handle non-utf8 paths
                let url = format!("https://{}", path.as_str().unwrap());
                debug!("Fetching package: git clone {} {} [version={}]",
                        url, clone_target.display(), crateid.version_or_default());

                let mut failed = false;

                cond.trap(|_| {
                    failed = true;
                }).inside(|| git_clone_url(url, &clone_target, &crateid.version));

                if failed {
                    return None;
                }

                // Move clone_target to local.
                // First, create all ancestor directories.
                let moved = make_dir_rwx_recursive(&local.dir_path())
                    && io::result(|| fs::rename(&clone_target, local)).is_ok();
                if moved { Some(local.clone()) }
                    else { None }
            }
        }
    }

    // If a file named "pkg.rs" in the start directory exists,
    // return the path for it. Otherwise, None
    pub fn package_script_option(&self) -> Option<Path> {
        let maybe_path = self.start_dir.join("pkg.rs");
        debug!("package_script_option: checking whether {} exists", maybe_path.display());
        if maybe_path.exists() {
            Some(maybe_path)
        } else {
            None
        }
    }

    pub fn push_crate(cs: &mut ~[Crate], prefix: uint, p: &Path) {
        let mut it = p.components().peekable();
        if prefix > 0 {
            it.nth(prefix-1); // skip elements
        }
        assert!(it.peek().is_some());
        let mut sub = Path::new(".");
        for c in it {
            sub.push(c);
        }
        debug!("Will compile crate {}", sub.display());
        cs.push(Crate::new(&sub));
    }

    /// Infers crates to build. Called only in the case where there
    /// is no custom build logic
    pub fn find_crates(&mut self) {
        self.find_crates_with_filter(|_| true);
    }

    pub fn find_crates_with_filter(&mut self, filter: |&str| -> bool) {
        use conditions::missing_pkg_files::cond;

        let prefix = self.start_dir.components().len();
        debug!("Matching against {}", self.id.name);
        for pth in fs::walk_dir(&self.start_dir) {
            let maybe_known_crate_set = match pth.filename_str() {
                Some(filename) if filter(filename) => match filename {
                    "lib.rs" => Some(&mut self.libs),
                    "main.rs" => Some(&mut self.mains),
                    "test.rs" => Some(&mut self.tests),
                    "bench.rs" => Some(&mut self.benchs),
                    _ => None
                },
                _ => None
            };

            match maybe_known_crate_set {
                Some(crate_set) => PkgSrc::push_crate(crate_set, prefix, &pth),
                None => ()
            }
        }

        let crate_sets = [&self.libs, &self.mains, &self.tests, &self.benchs];
        if crate_sets.iter().all(|crate_set| crate_set.is_empty()) {

            note("Couldn't infer any crates to build.\n\
                         Try naming a crate `main.rs`, `lib.rs`, \
                         `test.rs`, or `bench.rs`.");
            cond.raise(self.id.clone());
        }

        debug!("In {}, found {} libs, {} mains, {} tests, {} benchs",
               self.start_dir.display(),
               self.libs.len(),
               self.mains.len(),
               self.tests.len(),
               self.benchs.len())
    }

    fn build_crates(&self,
                    ctx: &BuildContext,
                    deps: &mut DepMap,
                    crates: &[Crate],
                    cfgs: &[~str],
                    what: OutputType,
                    inputs_to_discover: &[(~str, Path)]) {
        for crate in crates.iter() {
            let path = self.start_dir.join(&crate.file);
            debug!("build_crates: compiling {}", path.display());
            let cfgs = crate.cfgs + cfgs;

            ctx.workcache_context.with_prep(crate_tag(&path), |prep| {
                debug!("Building crate {}, declaring it as an input", path.display());
                // FIXME (#9639): This needs to handle non-utf8 paths
                prep.declare_input("file", path.as_str().unwrap(),
                                   workcache_support::digest_file_with_date(&path));
                let subpath = path.clone();
                let subcfgs = cfgs.clone();
                let subcx = ctx.clone();
                let id = self.id.clone();
                let sub_dir = self.build_workspace().clone();
                let sub_flags = crate.flags.clone();
                let sub_deps = deps.clone();
                let inputs = inputs_to_discover.map(|&(ref k, ref p)|
                                                    (k.clone(), p.as_str().unwrap().to_owned()));
                prep.exec(proc(exec) {
                    for &(ref kind, ref p) in inputs.iter() {
                        let pth = Path::new(p.clone());
                        exec.discover_input(*kind, *p, if *kind == ~"file" {
                                digest_file_with_date(&pth)
                            } else if *kind == ~"binary" {
                                digest_only_date(&Path::new(p.clone()))
                            } else {
                                fail!("Bad kind in build_crates")
                            });
                    }
                    debug!("Compiling crate {}; its output will be in {}",
                           subpath.display(), sub_dir.display());
                    let opt: session::OptLevel = subcx.context.rustc_flags.optimization_level;
                    let result = compile_crate(&subcx,
                                               exec,
                                               &id,
                                               &subpath,
                                               &sub_dir,
                                               &mut (sub_deps.clone()),
                                               sub_flags,
                                               subcfgs,
                                               opt,
                                               what);
                    // XXX: result is an Option<Path>. The following code did not take that
                    // into account. I'm not sure if the workcache really likes seeing the
                    // output as "Some(\"path\")". But I don't know what to do about it.
                    // FIXME (#9639): This needs to handle non-utf8 paths
                    let result = result.as_ref().map(|p|p.as_str().unwrap());
                    debug!("Result of compiling {} was {}", subpath.display(), result.to_str());
                    result.to_str()
                })
            });
        }
    }

    /// Declare all the crate files in the package source as inputs
    /// (to the package)
    pub fn declare_inputs(&self, prep: &mut workcache::Prep) {
        let to_do = ~[self.libs.clone(), self.mains.clone(),
                      self.tests.clone(), self.benchs.clone()];
        debug!("In declare inputs, self = {}", self.to_str());
        for cs in to_do.iter() {
            for c in cs.iter() {
                let path = self.start_dir.join(&c.file);
                debug!("Declaring input: {}", path.display());
                // FIXME (#9639): This needs to handle non-utf8 paths
                prep.declare_input("file", path.as_str().unwrap(),
                                   workcache_support::digest_file_with_date(&path.clone()));
            }
        }
    }

    pub fn build(&self,
                 build_context: &BuildContext,
                 // DepMap is a map from str (crate name) to (kind, name) --
                 // it tracks discovered dependencies per-crate
                 cfgs: ~[~str],
                 inputs_to_discover: &[(~str, Path)]) -> DepMap {
        let mut deps = TreeMap::new();
        let libs = self.libs.clone();
        let mains = self.mains.clone();
        let tests = self.tests.clone();
        let benchs = self.benchs.clone();
        debug!("Building libs in {}, destination = {}",
               self.source_workspace.display(),
               self.build_workspace().display());
        self.build_crates(build_context,
                          &mut deps,
                          libs,
                          cfgs,
                          Lib,
                          inputs_to_discover);
        debug!("Building mains");
        self.build_crates(build_context,
                          &mut deps,
                          mains,
                          cfgs,
                          Main,
                          inputs_to_discover);
        debug!("Building tests");
        self.build_crates(build_context,
                          &mut deps,
                          tests,
                          cfgs,
                          Test,
                          inputs_to_discover);
        debug!("Building benches");
        self.build_crates(build_context,
                          &mut deps,
                          benchs,
                          cfgs,
                          Bench,
                          inputs_to_discover);
        deps
    }

    /// Return the workspace to put temporary files in. See the comment on `PkgSrc`
    pub fn build_workspace<'a>(&'a self) -> &'a Path {
        if self.build_in_destination {
            &self.destination_workspace
        }
        else {
            &self.source_workspace
        }
    }

    /// Debugging
    pub fn dump_crates(&self) {
        let crate_sets = [&self.libs, &self.mains, &self.tests, &self.benchs];
        for crate_set in crate_sets.iter() {
            for c in crate_set.iter() {
                debug!("Built crate: {}", c.file.display())
            }
        }
    }
}
