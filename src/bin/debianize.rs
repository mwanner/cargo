use cargo::ops;
use cargo::util::important_paths::find_root_manifest_for_cwd;
use cargo::util::{CliResult, CliError, Config};

#[derive(RustcDecodable)]
struct Options {
    flag_verbose: bool,
    flag_manifest_path: Option<String>
}

pub const USAGE: &'static str = "
Create or update packaging for Debian

Usage:
    cargo debianize [options]

Options:
    -h, --help               Print this message
    --manifest-path PATH     Path to the manifest to debianize
    -v, --verbose            Use verbose output

Uses crago information to setup an initial debian directory used to
package a rust library or binary for Debian. Doesn't ever override a
file if it already exists.
";

pub fn execute(options: Options, config: &Config) -> CliResult<Option<()>> {
    config.shell().set_verbose(options.flag_verbose);
    let root = try!(find_root_manifest_for_cwd(options.flag_manifest_path));

    let opts = ops::DebianizeOptions {
        config: config,
    };

    match ops::debianize(&root, &opts) {
        Ok(_) => Ok(None),
        Err(e) => Err(CliError::from_boxed(e, 101))
    }
}
