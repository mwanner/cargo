use std::collections::HashMap;

use log::LogLevel::*;

use std::io::Write;
use std::path::Path;
use std::fs;
use std::fs::{File, PathExt};

use core::{Source, dependency};
use sources::{PathSource};
use util::config::Config;
use util::{CargoResult, human};

use debian::package::{Changelog, ChangelogEntry,
                      Dependency, SingleDependency, VRel,
                      ControlParagraph, ControlFile,
                      get_default_maintainer_name,
                      get_default_maintainer_email,
                      parse_dep_list};
use debian::Version;

pub struct DebianizeOptions<'a, 'b: 'a> {
    pub config: &'a Config<'b>
}

pub fn xform_pkg_name(cargo_name: &str) -> String {
    let is_system_pkg = cargo_name.len() > 4
        && cargo_name[0 .. cargo_name.len() - 4].as_slice() == "-sys";
    let has_rustc_prefix = cargo_name.len() > 5
        && cargo_name[0 .. 5].as_slice() == "rustc";

    // Special cases
    if cargo_name == "libc" {
        return "libc-rust".to_string();
    }

    // Usually prepending rust-, except for packages that wrap system
    // libraries (from other languages). For these, we use the common
    // lib prefix and append -rust.
    //
    // Some examples: rust-glob, rust-hamcrest, but libopenssl-rust.

    return match (is_system_pkg, has_rustc_prefix) {
        (true, false) => format!("lib{}-rust", cargo_name),
        (true, true) => panic!("does this make any sense?"),
        (false, false) => format!("rust-{}", cargo_name),
        (false, true) => cargo_name.to_string(),
    };
}

pub struct MakefileRule {
    target: String,
    deps: Vec<String>,
    rules: Vec<String>
}

impl MakefileRule {
    fn new(target: String) -> MakefileRule {
        MakefileRule {
            target: target,
            deps: vec![],
            rules: vec![]
        }
    }

    fn singleton(target: String, dep: String) -> MakefileRule {
        MakefileRule {
            target: target,
            deps: vec![dep],
            rules: vec![]
        }
    }

    fn add_rule(&mut self, r: String) -> &mut MakefileRule {
        self.rules.push(r);
        self
    }

    fn add_dep(&mut self, d: String) -> &mut MakefileRule {
        self.deps.push(d);
        self
    }

    fn serialize(&self) -> String {
        format!("{}: {}\n\t{}",
                self.target, self.deps.connect(" "),
                self.rules.connect("\n\t"))
    }
}

pub fn debianize(manifest_path: &Path,
                 options: &DebianizeOptions)
                 -> CargoResult<()>
{
    log!(Info, "debianize; manifest-path={}", manifest_path.display());

    let mut source = try!(PathSource::for_path(manifest_path.parent().unwrap(),
                                               options.config));
    try!(source.update());

    // TODO: Move this into PathSource (?)
    let package = try!(source.root_package());
    debug!("loaded package; package={}", package);

    for key in package.manifest().warnings().iter() {
        try!(options.config.shell().warn(key))
    }

    let root = package.package_id().source_id().clone();
    debug!("root: {}", root);
    debug!("cargo name: {}, version: {}", package.name(),
           package.version());

    let cargo_metadata = package.manifest().metadata();
    //let cargo_license = cargo_metadata.license.clone();
    //let cargo_license_file = cargo_metadata.license.clone();
    let cargo_homepage = cargo_metadata.homepage.clone();
    let cargo_repo = cargo_metadata.repository.clone();
    let cargo_desc = cargo_metadata.description.clone();
    let cargo_targets = package.targets();
    
    let dpkg_source_name = xform_pkg_name(package.name());
    let dpkg_version = package.version().to_string() + "-1";

    let deb_dir = manifest_path.parent().unwrap().join("debian");

    // Create the 'debian' directory, if it doesn't exist. Otherwise
    // check if it's a directory.
    if !deb_dir.exists() {
        match fs::create_dir(&deb_dir) {
            Ok(_) => {},
            Err(e) => return Err(human(
                format!("Unable to create the debian directory: {}", e)
                    .to_string()))
        }
        debug!("Created the 'debian' directory - it didn't exist before.");
    } else if !deb_dir.is_dir() {
        return Err(human(
            format!("Expected a directory, but {} is a file.",
                    deb_dir.display())));
    }

    // Update or create 'debian/changelog'.
    let deb_changelog = {
        let mut x = deb_dir.clone();
        x.push("changelog");
        x
    };

    if deb_changelog.exists() {
        panic!("Updating changelog not implemented, yet.");
    } else {
        let detail = "  * Initial debianization by cargo.\n".to_string();

        let e = ChangelogEntry::new(dpkg_source_name.clone(),
                                    dpkg_version, detail);
        let changelog = Changelog::new(e);
        match changelog.to_file(&deb_changelog) {
            Ok(_) => {},
            Err(e) => return Err(human(e))
        };
    }

    let deb_control = {
        let mut x = deb_dir.clone();
        x.push("control");
        x
    };

    let mut gp : ControlParagraph;
    if deb_control.exists() {
        let cf = match ControlFile::from_file(&deb_control) {
            Ok(f) => f,
            Err(e) => return Err(human(e))
        };

        let first_para = cf.get_paragraphs().get(0);
        if first_para.is_some() {
            gp = (*first_para.unwrap()).clone();
        } else {
            // Not even a first paragraph! Regenerate the control
            // file.
            //
            // FIXME: emit a warning
            gp = ControlParagraph::new();
            gp.add_entry("Source", dpkg_source_name.clone());
        };
    } else {
        gp = ControlParagraph::new();
        gp.add_entry("Source", dpkg_source_name.clone());
    }

    if !gp.has_entry("Priority") {
        gp.add_entry("Priority", "optional".to_string());
    }

    if !gp.has_entry("Section") {
        gp.add_entry("Section", "rust".to_string());
    }

    if !gp.has_entry("Maintainer") {
        gp.add_entry("Maintainer",
                     "Debian Rust Team <rust@debian.org>".to_string());
    }

    if !gp.has_entry("Uploaders") {
        gp.add_entry("Uploaders", format!("{} <{}>",
			get_default_maintainer_name(),
			get_default_maintainer_email()));
    }

    if !gp.has_entry("Standards-Version") {
        gp.add_entry("Standards-Version", "3.9.6".to_string());
    }

    // Synchronize Build-Depends
    {
        let cur_bd = match gp.get_entry("Build-Depends") {
            Some(val) => match parse_dep_list(val) {
                Ok(list) => list,
                Err(e) => return Err(human(e))
            },
            None => vec![]
        };
        let cur_bdi = match gp.get_entry("Build-Depends-Indep") {
            Some(val) => match parse_dep_list(val) {
                Ok(list) => list,
                Err(e) => return Err(human(e))
            },
            None => vec![]
        };
        let mut dep_map : HashMap<String, &SingleDependency> = HashMap::new();
        for d in cur_bd.iter().chain(cur_bdi.iter()) {
            for dep in d.alternatives.iter() {
                dep_map.insert(dep.package.clone(), dep);
            }
        }

        // First, check for build dependencies required by debian to
        // build from the debianized variant.
        let mut new_bd = cur_bd.clone();
        if !dep_map.contains_key("debhelper") {
            let dep = Dependency { alternatives: vec![
                SingleDependency {
                    package: "debhelper".to_string(),
                    version: Some((VRel::GreaterOrEqual,
                                   Version::parse("9.20150101.1+nmu1").ok().unwrap())),
                    arch: None
                }
            ]};
            new_bd.push(dep);
        }

        if !dep_map.contains_key("rustc") {
            let dep = Dependency { alternatives: vec![
                SingleDependency {
                    package: "rustc".to_string(),
                    version: None,
                    arch: None
                }
            ]};
            new_bd.push(dep);
        }

        // Then, check against the dependencies from Cargo.
        for dep in package.dependencies().iter() {
            let deb_name = xform_pkg_name(dep.name());
            debug!("  dependency: {} - dpkg: {}", dep.name(), deb_name);

            if dep.is_optional() {
                debug!("     optional");
            }

            match dep.kind() {
                dependency::Kind::Normal => debug!("      normal dep"),
                dependency::Kind::Development => debug!("      development dep"),
                dependency::Kind::Build => debug!("      build dep"),
            }

            debug!("    version req: {}", dep.version_req());

            for f in dep.features().iter() {
                debug!("    feature: {:?}", f);
            }

            debug!("    source_id: {:?}", dep.source_id());

            // We simply skip dev dependencies relative to the current
            // directory.
            if dep.source_id().is_path() &&
                dep.kind() == dependency::Kind::Development {
                    continue;
            }
            

            match dep_map.get(&deb_name) {
                Some(dep) => {
                    debug!("Already contains build dependency {}: {:?}.", deb_name, dep);
                },
                None => {
                    let dep = Dependency { alternatives: vec![
                        SingleDependency {
                            package: format!("{}-dev", deb_name),
                            version: None,
                            arch: None
                        }
                    ]};
                    new_bd.push(dep);
                }
            }
        }

        gp.update_entry("Build-Depends", new_bd.iter()
                        .map(|x| format!("{}", x))
                        .collect::<Vec<String>>()
                        .connect(", "));
    }

    // We always override repository and homepage info.
    match cargo_repo {
        Some(val) => {
            gp.update_entry("Vcs-Git", val.clone());
            gp.update_entry("Vcs-Browser", val);
        }
        None => {}
    };

    match cargo_homepage {
        Some(val) => { gp.update_entry("Homepage", val); }
        None => { }
    };




    
    let mut cf = ControlFile::new();
    cf.add_paragraph(gp);




    let /* mut */ release_targets = cargo_targets.iter()
        .filter(|target| target.profile().env() == "release");


    let mut mk_rules = vec![];
    let mut target_libs = vec![]; // libs to install
    let mut all_targets = vec![]; // what 'all' needs to build
    for target in cargo_targets.iter().filter(|tgt|
            tgt.is_lib() && tgt.profile().env() == "release") {
        let metadata = target.metadata().unwrap();
        let stamp = "build/lib".to_string() + target.name() + ".stamp";

        // FIXME: maybe reunite with root_path from
        // cargo_rustc/mod.rs, where this is stolen from.
        let absolute = package.root().join(target.src_path());
        let cwd = manifest_path.parent().unwrap();
        let crate_src_path = if absolute.starts_with(cwd) {
            absolute.relative_from(cwd).map(|s| s.to_path_buf()).unwrap_or(absolute)
        } else {
            absolute
        };

        let mut r = MakefileRule::new(stamp.clone());
        // fixme: dependencies
        r.add_rule("@if test ! -d build; then mkdir build; fi".to_string());
        r.add_rule(format!("rustc {} --crate-name {} --crate-type staticlib,rlib,dylib -C prefer-dynamic -C opt-level=3 --cfg ndebug -C metadata={} -C extra-filename={} --out-dir build --emit=dep-info,link",
                          crate_src_path.display(),
                          target.name(),
                          metadata.metadata,
                          metadata.extra_filename
                           ));
        r.add_rule(format!("touch {}", stamp.clone()));
        mk_rules.push(r);

        let dylib_filename = "build/lib".to_string() + target.name() +
            metadata.extra_filename.as_slice() + ".so";
        mk_rules.push(MakefileRule::singleton(dylib_filename.clone(),
                                              stamp.clone()));
        target_libs.push(dylib_filename);

        let rlib_filename = "build/lib".to_string() + target.name() +
            metadata.extra_filename.as_slice() + ".rlib";
        mk_rules.push(MakefileRule::singleton(rlib_filename.clone(),
                                              stamp.clone()));
        target_libs.push(rlib_filename);

        let staticlib_filename = "build/lib".to_string() + target.name() +
            metadata.extra_filename.as_slice() + ".a";
        mk_rules.push(MakefileRule::singleton(staticlib_filename.clone(),
                                              stamp.clone()));
        target_libs.push(staticlib_filename);

        all_targets.push(stamp);

        // Add control paragraphs for the dylib and a separate -dev
        // package with the rlib and the static library.
        let long_desc = match &cargo_desc {
            &Some(ref s) => Some(s.trim().split('\n')
                                 .map(|s| s.to_string())
                                 .collect::<Vec<String>>()
                                 .connect("\n ")),
            &None => None
        };
        
        let mut lp = ControlParagraph::new();
        lp.add_entry("Package",
                     format!("{}-{}", dpkg_source_name,
                             package.version()));
        lp.add_entry("Architecture", "amd64 i386".to_string());
        lp.add_entry("Pre-Depends", "${misc:Pre-Depends}".to_string());
        lp.add_entry("Depends",
                     "${misc:Depends}, ${shlibs:Depends}".to_string());
        // Recommends, Suggests ??

        lp.add_entry("Description", dpkg_source_name.clone() +
                     "rust crate - dylib" +
                     match &long_desc {
                         &Some(ref s) => ("\n ".to_string() + s.as_slice() +
                     "\n .\n This package contains the dynamic library."),
                         &None => "".to_string()
                     }.as_slice());
        cf.add_paragraph(lp);

        let mut lp = ControlParagraph::new();
        lp.add_entry("Package", dpkg_source_name.clone() + "-dev");
        lp.add_entry("Architecture", "amd64 i386".to_string());
        lp.add_entry("Pre-Depends", "${misc:Pre-Depends}".to_string());
        lp.add_entry("Depends",
                     "${misc:Depends}, ${shlibs:Depends}".to_string());
        // Recommends, Suggests ??

        lp.add_entry("Description", dpkg_source_name.clone() +
                     "rust crate - rlib and staticlib" +
                     match &long_desc {
                         &Some(ref s) => ("\n ".to_string() + s.as_slice() +
                     "\n .\n This package contains the static and rlib variants of the library."),
                         &None => "".to_string()
                     }.as_slice());
        cf.add_paragraph(lp);


        // Generate .install files
        let deb_lib_install = deb_dir.join(&format!("{}-{}.install",
                                                    dpkg_source_name,
                                                    package.version())[..]);
        {
            let mut f = match File::create(&deb_lib_install) {
                Ok(f) => f,
                Err(e) => return Err(human(e))
            };

            mk_rules.reverse();
            match f.write(format!("/usr/lib/x86_64-linux-gnu/rust/1.0/lib/rustlib/x86_64-unknown-linux-gnu/lib/lib{}-*.so\n", target.name()).as_bytes()) {
                Ok(_) => {},
                Err(e) => return Err(human(e))
            };
        }

        let deb_dev_install = deb_dir.join(&format!("{}-dev.install",
                                                    dpkg_source_name)[..]);
        {
            let mut f = match File::create(&deb_dev_install) {
                Ok(f) => f,
                Err(e) => return Err(human(e))
            };

            mk_rules.reverse();
            match f.write(format!("/usr/lib/x86_64-linux-gnu/rust/1.0/lib/rustlib/x86_64-unknown-linux-gnu/lib/lib{}-*.rlib\n/usr/lib/x86_64-linux-gnu/rust/1.0/lib/rustlib/x86_64-unknown-linux-gnu/lib/lib{}-*.a\n", target.name(), target.name()).as_bytes()) {
                Ok(_) => {},
                Err(e) => return Err(human(e))
            };
        }
    }


    // Add a 'check' target - FIXME: not currently functional
    {
        let mut r = MakefileRule::new("check".to_string());
        r.add_dep("all".to_string());
        // r.add_dep("test_programs".to_string());

        // FIXME: actually run the tests

        mk_rules.push(r);
    }


    
    // Add the 'all' and 'install' targets.
    {
        let mut r = MakefileRule::new("install".to_string());
        r.add_dep("all".to_string());
        r.add_rule("install -d $(DESTDIR)/usr/lib/x86_64-linux-gnu/rust/1.0/lib/rustlib/x86_64-unknown-linux-gnu/lib/".to_string());
        for lib in target_libs.into_iter() {
            r.add_rule(format!("install -m 644 -s {} $(DESTDIR)/usr/lib/x86_64-linux-gnu/rust/1.0/lib/rustlib/x86_64-unknown-linux-gnu/lib/", lib));
        }
        mk_rules.push(r);
        
        let mut r = MakefileRule::new("all".to_string());
        for dep in all_targets.into_iter() {
            r.add_dep(dep);
        }
        mk_rules.push(r);
    }

    let deb_makefile = deb_dir.join("Makefile.cargo");
    {
        let mut f = match File::create(&deb_makefile) {
            Ok(f) => f,
            Err(e) => return Err(human(e))
        };

        mk_rules.reverse();
        let rules = mk_rules.iter().map(|r| r.serialize())
            .collect::<Vec<String>>().connect("\n\n");
        match f.write(format!("#!/usr/bin/make -f

# Automatically generated by cargo. DO NOT EDIT.

{}
", rules).as_bytes()) {
            Ok(_) => {},
            Err(e) => return Err(human(e))
        };
    }




    




    
    /*
    let mut test_targets = cargo_targets.iter()
        .filter(|target| target.profile().env() == "test");
    let mut doc_targets = cargo_targets.iter()
        .filter(|target| target.profile().env() == "doc");
     */
    for target in release_targets {
        debug!("tgt name: {}, src path: {:?}, metadata: {:?}, profile: {:?}", target.name(),
               target.src_path(), target.metadata(), target.profile());

        if target.is_lib() {
            
        } else if target.is_bin() {
        } else if target.is_example() {
        } else {
            unreachable!();
        }
        

        // is_lib, is_dylib, is_rlib, is_staticlib, is_bin, is_example



/*
        let cx = 0;
        let req = 0;
        rustc(package, target, cx, req);
*/
    }


    let deb_compat = deb_dir.join("compat");
    if !deb_compat.exists() {
        let mut f = match File::create(&deb_compat) {
            Ok(f) => f,
            Err(e) => return Err(human(e))
        };
        match f.write("9\n".as_bytes()) {
            Ok(_) => {},
            Err(e) => return Err(human(e))
        };
    }


    let deb_source_dir = deb_dir.join("source");
    let deb_source_format = deb_source_dir.join("format");
    if !deb_source_dir.exists() {
        match fs::create_dir(&deb_source_dir) {
            Ok(_) => {},
            Err(e) => return Err(human(
                format!("Unable to create the debian/source directory: {}", e)
                    .to_string()))
        }
        debug!("Created the 'debian/source' directory - it didn't exist before.");
    }

    if !deb_source_format.exists() {
        let mut f = match File::create(&deb_source_format) {
            Ok(f) => f,
            Err(e) => return Err(human(e))
        };
        match f.write("3.0 (quilt)\n".as_bytes()) {
            Ok(_) => {},
            Err(e) => return Err(human(e))
        };
    }




    let deb_rules = deb_dir.join("rules");
    if !deb_rules.exists() {
        {
            let mut f = match File::create(&deb_rules) {
                Ok(f) => f,
                Err(e) => return Err(human(e))
            };
            match f.write("#!/usr/bin/make -f

%:
\tdh $@
".as_bytes()) {
                Ok(_) => {},
                Err(e) => return Err(human(e))
            };
        }

/* FIXME: mark executable
        match fs::chmod(&deb_rules, old_io::USER_EXEC) {
            Ok(_) => { },
            Err(e) => return Err(human(e))
        }
*/
    }
    


    return match cf.serialize(&deb_control) {
        Ok(_) => Ok(()),
        Err(e) => Err(human(format!("Error writing control file: {}", e)))
    };
}
