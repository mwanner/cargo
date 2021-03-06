use std::io::prelude::*;
use std::fs::{self, File};
use std::io::{Cursor, SeekFrom};
use std::path::PathBuf;

use flate2::read::GzDecoder;
use tar::Archive;
use url::Url;

use support::{project, execs};
use support::{UPDATING, PACKAGING, UPLOADING};
use support::paths;
use support::git::repo;

use hamcrest::assert_that;

fn registry_path() -> PathBuf { paths::root().join("registry") }
fn registry() -> Url { Url::from_file_path(&*registry_path()).ok().unwrap() }
fn upload_path() -> PathBuf { paths::root().join("upload") }
fn upload() -> Url { Url::from_file_path(&*upload_path()).ok().unwrap() }

fn setup() {
    let config = paths::root().join(".cargo/config");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    File::create(&config).unwrap().write_all(&format!(r#"
        [registry]
            index = "{reg}"
            token = "api-token"
    "#, reg = registry()).as_bytes()).unwrap();
    fs::create_dir_all(&upload_path().join("api/v1/crates")).unwrap();

    repo(&registry_path())
        .file("config.json", &format!(r#"{{
            "dl": "{0}",
            "api": "{0}"
        }}"#, upload()))
        .build();
}

test!(simple {
    let p = project("foo")
        .file("Cargo.toml", r#"
            [project]
            name = "foo"
            version = "0.0.1"
            authors = []
            license = "MIT"
            description = "foo"
        "#)
        .file("src/main.rs", "fn main() {}");

    assert_that(p.cargo_process("publish").arg("--no-verify"),
                execs().with_status(0).with_stdout(format!("\
{updating} registry `{reg}`
{packaging} foo v0.0.1 ({dir})
{uploading} foo v0.0.1 ({dir})
",
        updating = UPDATING,
        uploading = UPLOADING,
        packaging = PACKAGING,
        dir = p.url(),
        reg = registry()).as_slice()));

    let mut f = File::open(&upload_path().join("api/v1/crates/new")).unwrap();
    // Skip the metadata payload and the size of the tarball
    let mut sz = [0; 4];
    assert_eq!(f.read(&mut sz), Ok(4));
    let sz = ((sz[0] as u32) <<  0) |
             ((sz[1] as u32) <<  8) |
             ((sz[2] as u32) << 16) |
             ((sz[3] as u32) << 24);
    f.seek(SeekFrom::Current(sz as i64 + 4)).unwrap();

    // Verify the tarball
    let mut rdr = GzDecoder::new(f).unwrap();
    assert_eq!(rdr.header().filename(), Some(b"foo-0.0.1.crate"));
    let mut contents = Vec::new();
    rdr.read_to_end(&mut contents).unwrap();
    let inner = Cursor::new(contents);
    let ar = Archive::new(inner);
    for file in ar.files().unwrap() {
        let file = file.unwrap();
        let fname = file.filename_bytes();
        assert!(fname == b"foo-0.0.1/Cargo.toml" ||
                fname == b"foo-0.0.1/src/main.rs",
                "unexpected filename: {:?}", file.filename())
    }
});

test!(git_deps {
    let p = project("foo")
        .file("Cargo.toml", r#"
            [project]
            name = "foo"
            version = "0.0.1"
            authors = []
            license = "MIT"
            description = "foo"

            [dependencies.foo]
            git = "git://path/to/nowhere"
        "#)
        .file("src/main.rs", "fn main() {}");

    assert_that(p.cargo_process("publish").arg("-v").arg("--no-verify"),
                execs().with_status(101).with_stderr("\
all dependencies must come from the same source.
dependency `foo` comes from git://path/to/nowhere instead
"));
});

test!(path_dependency_no_version {
    let p = project("foo")
        .file("Cargo.toml", r#"
            [project]
            name = "foo"
            version = "0.0.1"
            authors = []
            license = "MIT"
            description = "foo"

            [dependencies.bar]
            path = "bar"
        "#)
        .file("src/main.rs", "fn main() {}")
        .file("bar/Cargo.toml", r#"
            [package]
            name = "bar"
            version = "0.0.1"
            authors = []
        "#)
        .file("bar/src/lib.rs", "");

    assert_that(p.cargo_process("publish"),
                execs().with_status(101).with_stderr("\
all path dependencies must have a version specified when publishing.
dependency `bar` does not specify a version
"));
});
