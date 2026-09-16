#![allow(dead_code)]

use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    io::{Read, Write},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub const BASELINE: &str = "3f2e5d1b9acfbe4a4338bb003425668cc803464e";
pub const ADAPTER: &str = include_str!("../../../../../tests/go-oracle/adapter/main.go");
const TREE_MANIFEST: &str =
    include_str!("../../../../../tests/go-oracle/frozen-tree-manifest.json");
const PROXY_MANIFEST: &str =
    include_str!("../../../../../tests/go-oracle/module-proxy-manifest.json");
const TREE_MANIFEST_SHA256: &str =
    "d023cc24ccb51d9c2bc2039255fd74603e41bb1b0d11d4db198ae44e5acb17a4";
const PROXY_MANIFEST_SHA256: &str =
    "2ed831b658b0d90cc71b610054d19c6c54941bea3dfc2d95d3a73685be7e6668";
const REQUIRED_GO: (u64, u64, u64) = (1, 25, 4);
const FIXED_BUILD_ENVIRONMENT: [&str; 19] = [
    "HOME",
    "TMPDIR",
    "GOTMPDIR",
    "GOCACHE",
    "GOPATH",
    "GOMODCACHE",
    "GO111MODULE",
    "GOENV",
    "GOWORK",
    "GOSUMDB",
    "GOTOOLCHAIN",
    "GOTELEMETRY",
    "CGO_ENABLED",
    "TZ",
    "LANG",
    "LC_ALL",
    "PATH",
    "GOFLAGS",
    "GOPROXY",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeManifest {
    pub schema_version: u64,
    pub baseline_commit: String,
    pub roots: Vec<String>,
    pub go_mod_sha256: String,
    pub go_sum_sha256: String,
    pub adapter_sha256: String,
    pub files: Vec<TreeFile>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeFile {
    pub path: String,
    pub mode: String,
    pub object_type: String,
    pub git_blob: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyManifest {
    pub schema_version: u64,
    pub baseline_commit: String,
    pub go_mod_sha256: String,
    pub go_sum_sha256: String,
    pub generator_go_version: String,
    pub target_modules: Vec<String>,
    pub modules: Vec<ModuleRecord>,
    pub graph_mods: Vec<GraphMod>,
    pub files: Vec<ProxyFile>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphMod {
    pub path: String,
    pub version: String,
    pub go_mod_sum: String,
    pub authentication: String,
    pub proxy_path: String,
    pub mod_sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleRecord {
    pub path: String,
    pub version: String,
    #[serde(default)]
    pub sum: String,
    #[serde(default)]
    pub go_mod_sum: String,
    pub replacement: Option<ReplacementRecord>,
    #[serde(default)]
    pub info_sha256: String,
    #[serde(default)]
    pub mod_sha256: String,
    #[serde(default)]
    pub zip_sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplacementRecord {
    pub path: String,
    pub git_tree: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyFile {
    pub path: String,
    pub mode: String,
    pub sha256: String,
}

pub struct OracleFixture {
    outer: PathBuf,
    fixture: PathBuf,
    binary: PathBuf,
    module_cache: PathBuf,
    go_version: String,
}

impl OracleFixture {
    pub fn build() -> TestResult<Self> {
        require_sandbox()?;
        let outer = unique_root("oracle")?;
        let fixture = outer.join("fixture");
        fs::create_dir(&fixture)?;
        let result = build_in(&fixture);
        match result {
            Ok((binary, module_cache, go_version)) => Ok(Self {
                outer,
                fixture,
                binary,
                module_cache,
                go_version,
            }),
            Err(error) => {
                let _ = make_writable(&outer);
                let _ = fs::remove_dir_all(&outer);
                Err(error)
            }
        }
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    pub fn root(&self) -> &Path {
        &self.fixture
    }

    pub fn module_cache(&self) -> &Path {
        &self.module_cache
    }

    pub fn go_version(&self) -> &str {
        &self.go_version
    }

    pub fn run(
        &self,
        environment: &[(&str, String)],
        request: &[u8],
        timeout: Duration,
    ) -> TestResult<Output> {
        sandboxed_command(
            &self.binary,
            &[],
            environment,
            &self.fixture,
            Some(request),
            timeout,
        )
    }

    pub fn cleanup(self) -> TestResult {
        let outer = self.outer.clone();
        assert_no_surviving_processes(&self.fixture)?;
        assert_no_forbidden_artifacts(&outer)?;
        make_writable(&outer)?;
        fs::remove_dir_all(&outer)?;
        std::mem::forget(self);
        if outer.exists() {
            return Err(format!("oracle root survived cleanup: {}", outer.display()).into());
        }
        Ok(())
    }
}

impl Drop for OracleFixture {
    fn drop(&mut self) {
        let _ = make_writable(&self.outer);
        let _ = fs::remove_dir_all(&self.outer);
    }
}

pub fn attempt_dependency_materialization_with_fallback(
    fixture: &Path,
    proxy: &Path,
    fallback: &str,
) -> TestResult<Output> {
    require_sandbox()?;
    let source_root = fixture.join("acquisition-source");
    archive_source(BASELINE, &source_root)?;
    verify_archived_tree(&source_root)?;
    let go = find_go()?;
    let build = fixture.join("acquisition-build");
    for directory in ["home", "tmp", "gocache", "gopath"] {
        fs::create_dir_all(build.join(directory))?;
    }
    fs::create_dir_all(build.join("gopath/pkg/mod"))?;
    stage_sumdb(proxy, &build)?;
    let manifest = proxy_manifest(PROXY_MANIFEST)?;
    let external = manifest
        .modules
        .iter()
        .filter(|module| module.replacement.is_none())
        .map(|module| format!("{}@{}", module.path, module.version))
        .collect::<Vec<_>>();
    let mut arguments = vec![
        "-C".to_owned(),
        source_root
            .join("skills/orchestrator")
            .to_string_lossy()
            .into_owned(),
        "mod".to_owned(),
        "download".to_owned(),
    ];
    arguments.extend(external);
    let proxy_url = format!("file://{}|{fallback}", proxy.display());
    let environment = build_environment(&build, &go, &proxy_url, "sum.golang.org");
    sandboxed_command(
        &go,
        &arguments,
        &environment,
        fixture,
        None,
        Duration::from_secs(120),
    )
}

fn build_in(fixture: &Path) -> TestResult<(PathBuf, PathBuf, String)> {
    let repository = repository_root();
    verify_revision(BASELINE)?;
    let source_root = fixture.join("source");
    archive_source(BASELINE, &source_root)?;
    verify_archived_tree(&source_root)?;
    let adapter_target = source_root.join("skills/orchestrator/cmd/core-parity-oracle");
    if adapter_target.exists() {
        return Err("frozen archive unexpectedly contains the oracle command".into());
    }
    verify_adapter(ADAPTER.as_bytes())?;
    fs::create_dir_all(&adapter_target)?;
    fs::write(adapter_target.join("main.go"), ADAPTER)?;

    let committed_proxy = workspace_root().join("tests/go-oracle/module-proxy");
    verify_proxy(&committed_proxy, PROXY_MANIFEST)?;
    let proxy = fixture.join("sealed-proxy");
    copy_tree(&committed_proxy, &proxy)?;
    verify_proxy(&proxy, PROXY_MANIFEST)?;

    let go = find_go()?;
    let go_version_output = cleared_command(&go, &["version"], &repository)?;
    require_success("reading Go compiler version", &go_version_output)?;
    let go_version = String::from_utf8(go_version_output.stdout.clone())?
        .trim()
        .to_owned();
    require_go_version(&go_version)?;

    let build = fixture.join("build");
    for directory in ["home", "tmp", "gocache", "gopath", "bin"] {
        fs::create_dir_all(build.join(directory))?;
    }
    let module_cache = build.join("gopath/pkg/mod");
    fs::create_dir_all(&module_cache)?;
    stage_sumdb(&proxy, &build)?;
    let module_dir = source_root.join("skills/orchestrator");
    let proxy_manifest = proxy_manifest(PROXY_MANIFEST)?;
    let proxy_url = format!("file://{}", proxy.display());
    let materialize_env = build_environment(&build, &go, &proxy_url, "sum.golang.org");
    let external = proxy_manifest
        .modules
        .iter()
        .filter(|module| module.replacement.is_none())
        .map(|module| format!("{}@{}", module.path, module.version))
        .collect::<Vec<_>>();
    let mut download_args = vec![
        "-C".to_owned(),
        module_dir.to_string_lossy().into_owned(),
        "mod".to_owned(),
        "download".to_owned(),
    ];
    download_args.extend(external);
    let downloaded = sandboxed_command(
        &go,
        &download_args,
        &materialize_env,
        fixture,
        None,
        Duration::from_secs(180),
    )?;
    require_success("materializing sealed Go dependencies", &downloaded)?;
    let verified = sandboxed_command(
        &go,
        &[
            "-C".to_owned(),
            module_dir.to_string_lossy().into_owned(),
            "mod".to_owned(),
            "verify".to_owned(),
        ],
        &materialize_env,
        fixture,
        None,
        Duration::from_secs(120),
    )?;
    require_success("verifying sealed Go dependencies", &verified)?;

    let offline_env = build_environment(&build, &go, "off", "off");
    let listed = sandboxed_command(
        &go,
        &[
            "-C".to_owned(),
            module_dir.to_string_lossy().into_owned(),
            "list".to_owned(),
            "-deps".to_owned(),
            "-mod=readonly".to_owned(),
            "-f".to_owned(),
            "{{with .Module}}{{if and .Path .Version}}{{.Path}}@{{.Version}}{{end}}{{end}}"
                .to_owned(),
            "./cmd/core-parity-oracle".to_owned(),
        ],
        &offline_env,
        fixture,
        None,
        Duration::from_secs(120),
    )?;
    require_success("listing offline Go dependency graph", &listed)?;
    verify_selected_modules(&listed.stdout, &proxy_manifest)?;
    make_read_only(&module_cache)?;

    let binary = build.join("bin/core-parity-oracle");
    let built = sandboxed_command(
        &go,
        &[
            "-C".to_owned(),
            module_dir.to_string_lossy().into_owned(),
            "build".to_owned(),
            "-mod=readonly".to_owned(),
            "-trimpath".to_owned(),
            "-buildvcs=false".to_owned(),
            "-o".to_owned(),
            binary.to_string_lossy().into_owned(),
            "./cmd/core-parity-oracle".to_owned(),
        ],
        &offline_env,
        fixture,
        None,
        Duration::from_secs(180),
    )?;
    require_success("building offline read-only Go oracle", &built)?;
    if !binary.is_file() {
        return Err("offline Go build did not produce the oracle binary".into());
    }
    Ok((binary, module_cache, go_version))
}

pub fn verify_revision(revision: &str) -> TestResult {
    if revision != BASELINE
        || revision.len() != 40
        || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(format!("untrusted frozen revision {revision:?}").into());
    }
    let git = find_git()?;
    let (mut arguments, working_directory) = frozen_git_location()?;
    arguments.extend([
        "rev-parse".to_owned(),
        "--verify".to_owned(),
        format!("{revision}^{{commit}}"),
    ]);
    let output = cleared_owned_command(&git, &arguments, &working_directory)?;
    require_success("resolving frozen Go revision", &output)?;
    let found = String::from_utf8(output.stdout)?.trim().to_owned();
    if found != BASELINE {
        return Err(format!("frozen revision resolved to {found}, expected {BASELINE}").into());
    }
    Ok(())
}

pub fn archive_source(revision: &str, destination: &Path) -> TestResult {
    verify_revision(revision)?;
    fs::create_dir_all(destination)?;
    let archive = destination
        .parent()
        .ok_or("archive destination has no parent")?
        .join("frozen-source.tar");
    let manifest = tree_manifest()?;
    let git = find_git()?;
    let (mut arguments, working_directory) = frozen_git_location()?;
    arguments.extend([
        "archive".to_owned(),
        "--format=tar".to_owned(),
        "--output".to_owned(),
        archive.to_string_lossy().into_owned(),
        revision.to_owned(),
        "--".to_owned(),
    ]);
    arguments.extend(manifest.roots);
    let output = cleared_owned_command(&git, &arguments, &working_directory)?;
    require_success("archiving frozen Go source", &output)?;
    let tar = Path::new("/usr/bin/tar");
    if !tar.is_file() {
        return Err("allowlisted tar executable is unavailable".into());
    }
    let extracted = cleared_owned_command(
        tar,
        &[
            "-xf".to_owned(),
            archive.to_string_lossy().into_owned(),
            "-C".to_owned(),
            destination.to_string_lossy().into_owned(),
        ],
        destination,
    )?;
    require_success("extracting frozen Go source", &extracted)?;
    fs::remove_file(archive)?;
    Ok(())
}

pub fn verify_archived_tree(root: &Path) -> TestResult {
    verify_archived_tree_manifest(root, TREE_MANIFEST)
}

pub fn verify_archived_tree_manifest(root: &Path, manifest_bytes: &str) -> TestResult {
    if sha256_bytes(manifest_bytes.as_bytes())? != TREE_MANIFEST_SHA256 {
        return Err("frozen-tree manifest digest mismatch".into());
    }
    let manifest: TreeManifest = serde_json::from_str(manifest_bytes)?;
    if manifest.schema_version != 1 || manifest.baseline_commit != BASELINE {
        return Err("unsupported frozen-tree manifest identity".into());
    }
    if manifest.go_mod_sha256 != "d0cbdb2787c496b249098e7c0d12f2c01a1c2ed719301f19cffe6713c9c8429c"
        || manifest.go_sum_sha256
            != "15f3537bb9086e7ace3cf6d7a80c1bd4bfb125df33d612f9b8489cbf89a1e464"
    {
        return Err("frozen module metadata digests changed".into());
    }
    let root_set = manifest.roots.iter().cloned().collect::<BTreeSet<_>>();
    if root_set.len() != manifest.roots.len()
        || manifest
            .roots
            .iter()
            .any(|path| !safe_relative(Path::new(path)))
    {
        return Err("invalid or duplicate frozen archive roots".into());
    }
    let git_entries = frozen_git_entries(&manifest.roots)?;
    let actual = regular_files(root)?;
    let expected = manifest
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    if expected.len() != manifest.files.len() {
        return Err("duplicate frozen archive manifest path".into());
    }
    if actual != expected || git_entries.keys().cloned().collect::<BTreeSet<_>>() != expected {
        return Err(set_mismatch("frozen archive paths", &expected, &actual).into());
    }
    let paths = manifest
        .files
        .iter()
        .map(|file| root.join(&file.path))
        .collect::<Vec<_>>();
    let hashes = sha256_files(&paths)?;
    for (file, hash) in manifest.files.iter().zip(hashes) {
        let (git_mode, git_type, git_object) = git_entries
            .get(&file.path)
            .ok_or("authenticated Git entry disappeared")?;
        if &file.mode != git_mode || &file.object_type != git_type || &file.git_blob != git_object {
            return Err(format!("unsupported frozen object metadata for {}", file.path).into());
        }
        if file.sha256 != hash {
            return Err(format!("frozen archive content mismatch for {}", file.path).into());
        }
        let metadata = fs::symlink_metadata(root.join(&file.path))?;
        if !metadata.file_type().is_file() {
            return Err(
                format!("frozen archive entry is not a regular file: {}", file.path).into(),
            );
        }
        let found_mode = if metadata.permissions().mode() & 0o111 == 0 {
            "100644"
        } else {
            "100755"
        };
        if file.mode != found_mode {
            return Err(format!("frozen archive mode mismatch for {}", file.path).into());
        }
    }
    Ok(())
}

pub fn verify_adapter(bytes: &[u8]) -> TestResult {
    let manifest = tree_manifest()?;
    if sha256_bytes(bytes)? != manifest.adapter_sha256 {
        return Err("oracle adapter digest mismatch".into());
    }
    let source = std::str::from_utf8(bytes)?;
    for forbidden in [
        "\"net\"",
        "\"net/http\"",
        "\"os/exec\"",
        "\"syscall\"",
        "\"unsafe\"",
        "os.Create(",
        "os.OpenFile(",
        "os.WriteFile(",
        "os.Mkdir(",
        "os.MkdirAll(",
        "os.Remove(",
        "os.RemoveAll(",
        "os.Rename(",
        "os.Chmod(",
        "os.Chown(",
        "os.Truncate(",
        "os.Link(",
        "os.Symlink(",
        "os.Pipe(",
        "os.StartProcess(",
        "exec.Command(",
    ] {
        if source.contains(forbidden) {
            return Err(format!("oracle adapter contains forbidden effect {forbidden:?}").into());
        }
    }
    Ok(())
}

pub fn verify_proxy(root: &Path, manifest_bytes: &str) -> TestResult {
    if sha256_bytes(manifest_bytes.as_bytes())? != PROXY_MANIFEST_SHA256 {
        return Err("sealed proxy manifest digest mismatch".into());
    }
    let manifest = proxy_manifest(manifest_bytes)?;
    let tree = tree_manifest()?;
    if manifest.schema_version != 1
        || manifest.baseline_commit != BASELINE
        || manifest.go_mod_sha256 != tree.go_mod_sha256
        || manifest.go_sum_sha256 != tree.go_sum_sha256
        || !manifest.generator_go_version.starts_with("go version go")
    {
        return Err("sealed proxy manifest identity mismatch".into());
    }
    let actual = regular_files(root)?;
    let expected = manifest
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    if expected.len() != manifest.files.len() {
        return Err("duplicate sealed proxy manifest path".into());
    }
    if actual != expected {
        return Err(set_mismatch("sealed proxy paths", &expected, &actual).into());
    }
    let paths = manifest
        .files
        .iter()
        .map(|file| root.join(&file.path))
        .collect::<Vec<_>>();
    let hashes = sha256_files(&paths)?;
    let by_path = manifest
        .files
        .iter()
        .zip(hashes)
        .map(|(file, hash)| {
            if file.mode != "0644" || file.sha256 != hash {
                Err(format!("sealed proxy file mismatch for {}", file.path))
            } else {
                Ok((file.path.clone(), hash))
            }
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    if by_path.len() != manifest.files.len() {
        return Err("duplicate sealed proxy file record".into());
    }
    let frozen_sums = frozen_go_sums()?;
    let mut module_keys = BTreeSet::new();
    for module in &manifest.modules {
        if module.path.is_empty()
            || module.version.is_empty()
            || !module_keys.insert(format!("{}@{}", module.path, module.version))
        {
            return Err("invalid or duplicate sealed proxy module".into());
        }
        if let Some(replacement) = &module.replacement {
            if !module.sum.is_empty()
                || !module.go_mod_sum.is_empty()
                || !module.info_sha256.is_empty()
                || !module.mod_sha256.is_empty()
                || !module.zip_sha256.is_empty()
                || !safe_relative(Path::new(&replacement.path))
                || replacement.git_tree != frozen_git_object(&replacement.path)?
            {
                return Err(
                    format!("invalid local replacement metadata for {}", module.path).into(),
                );
            }
            continue;
        }
        if !module.sum.starts_with("h1:")
            || !module.go_mod_sum.starts_with("h1:")
            || frozen_sums.get(&(module.path.clone(), module.version.clone())) != Some(&module.sum)
            || frozen_sums.get(&(module.path.clone(), format!("{}/go.mod", module.version)))
                != Some(&module.go_mod_sum)
        {
            return Err(format!("Go sum mismatch for {}", module.path).into());
        }
        let escaped = proxy_prefix(&manifest.files, module)?;
        for (extension, expected_hash) in [
            ("info", &module.info_sha256),
            ("mod", &module.mod_sha256),
            ("zip", &module.zip_sha256),
        ] {
            let path = format!("{escaped}.{extension}");
            if by_path.get(&path) != Some(expected_hash) {
                return Err(format!("sealed proxy module metadata mismatch for {path}").into());
            }
        }
    }
    let targets = manifest
        .target_modules
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if targets.len() != manifest.target_modules.len() || !targets.is_subset(&module_keys) {
        return Err("sealed proxy target-module set is invalid".into());
    }
    let mut graph_keys = BTreeSet::new();
    for module in &manifest.graph_mods {
        let authentication_valid = match module.authentication.as_str() {
            "frozen_go_sum" => {
                frozen_sums.get(&(module.path.clone(), format!("{}/go.mod", module.version)))
                    == Some(&module.go_mod_sum)
            }
            "sumdb" => {
                !frozen_sums
                    .contains_key(&(module.path.clone(), format!("{}/go.mod", module.version)))
                    && sumdb_lookup_contains(root, module)?
            }
            _ => false,
        };
        if module.path.is_empty()
            || module.version.is_empty()
            || !module.go_mod_sum.starts_with("h1:")
            || !authentication_valid
            || !graph_keys.insert(format!("{}@{}", module.path, module.version))
            || by_path.get(&module.proxy_path) != Some(&module.mod_sha256)
            || !module
                .proxy_path
                .ends_with(&format!("/@v/{}.mod", module.version))
        {
            return Err(format!("invalid module-graph metadata for {}", module.path).into());
        }
    }
    Ok(())
}

fn sumdb_lookup_contains(root: &Path, module: &GraphMod) -> TestResult<bool> {
    let lookup = root
        .join("sumdb/gomodcache/cache/download/sumdb/sum.golang.org/lookup")
        .join(format!(
            "{}@{}",
            escape_module_path(&module.path),
            module.version
        ));
    let expected = format!(
        "{} {}/go.mod {}",
        module.path, module.version, module.go_mod_sum
    );
    Ok(fs::read_to_string(lookup)?
        .lines()
        .any(|line| line == expected))
}

fn escape_module_path(path: &str) -> String {
    path.chars()
        .flat_map(|character| {
            if character.is_ascii_uppercase() {
                vec!['!', character.to_ascii_lowercase()]
            } else {
                vec![character]
            }
        })
        .collect()
}

fn proxy_prefix(files: &[ProxyFile], module: &ModuleRecord) -> TestResult<String> {
    let escaped = module
        .path
        .chars()
        .flat_map(|character| {
            if character.is_ascii_uppercase() {
                vec!['!', character.to_ascii_lowercase()]
            } else {
                vec![character]
            }
        })
        .collect::<String>();
    let prefix = format!("{escaped}/@v/{}", module.version);
    if !files
        .iter()
        .any(|file| file.path == format!("{prefix}.zip"))
    {
        return Err(format!("cannot resolve proxy path for {}", module.path).into());
    }
    Ok(prefix)
}

pub fn proxy_manifest_bytes() -> &'static str {
    PROXY_MANIFEST
}

pub fn tree_manifest_bytes() -> &'static str {
    TREE_MANIFEST
}

pub fn committed_proxy_root() -> PathBuf {
    workspace_root().join("tests/go-oracle/module-proxy")
}

pub fn copy_tree(source: &Path, destination: &Path) -> TestResult {
    if destination.exists() {
        fs::remove_dir_all(destination)?;
    }
    fs::create_dir_all(destination)?;
    for relative in regular_files(source)? {
        let source_path = source.join(&relative);
        let target = destination.join(&relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source_path, &target)?;
        // B5-DESIGN 9.J. `fs::copy` preserves the source mode, and under the
        // lease the source is the `chmod -R a-w` snapshot, so a scratch working
        // copy would come back with read-only files inside writable
        // directories. `yaml_patch_provenance.rs` carries the same three lines
        // for the same reason. No assertion depends on the mode: every verifier
        // over these trees compares content digests.
        let mut permissions = fs::metadata(&target)?.permissions();
        permissions.set_mode((permissions.mode() & 0o7777) | 0o200);
        fs::set_permissions(&target, permissions)?;
    }
    Ok(())
}

fn stage_sumdb(proxy: &Path, build: &Path) -> TestResult {
    for (source, destination) in [
        (proxy.join("sumdb/gomodcache"), build.join("gopath/pkg/mod")),
        (proxy.join("sumdb/gopath"), build.join("gopath")),
    ] {
        for relative in regular_files(&source)? {
            let target = destination.join(&relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(source.join(relative), target)?;
        }
    }
    Ok(())
}

pub fn validate_case_id(case_id: &str) -> TestResult<PathBuf> {
    let path = Path::new(case_id);
    if !safe_relative(path)
        || !case_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(format!("unsafe differential case id {case_id:?}").into());
    }
    Ok(path.to_path_buf())
}

pub fn validate_fixture_reference(reference: &str) -> TestResult<PathBuf> {
    let path = Path::new(reference);
    if !safe_relative(path) || reference.contains('$') || reference.contains('\0') {
        return Err(format!("unsafe fixture reference {reference:?}").into());
    }
    Ok(path.to_path_buf())
}

pub fn validate_case_environment(
    values: &serde_json::Map<String, serde_json::Value>,
    required: &[&str],
    root: &Path,
) -> TestResult<Vec<(String, String)>> {
    let required_set = required.iter().copied().collect::<BTreeSet<_>>();
    let actual_set = values.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if required_set != actual_set {
        return Err("case environment does not match the closed schema".into());
    }
    let mut result = Vec::new();
    for name in required {
        let value = values
            .get(*name)
            .ok_or("required environment key disappeared")?;
        if value.is_null() {
            continue;
        }
        let raw = value
            .as_str()
            .ok_or("environment values must be strings or null")?;
        if raw.matches("$FIXTURE_ROOT").count() > 1 || raw.contains("..") || raw.contains('\0') {
            return Err(format!("unsafe environment value for {name}").into());
        }
        let expanded = raw.replace("$FIXTURE_ROOT", &root.to_string_lossy());
        if expanded.contains('$') {
            return Err(format!("unresolved environment token for {name}").into());
        }
        if is_path_environment(name) {
            let path = Path::new(&expanded);
            validate_case_path(path, root)
                .map_err(|error| format!("unsafe environment path for {name}: {error}"))?;
        }
        result.push(((*name).to_owned(), expanded));
    }
    Ok(result)
}

pub fn validate_case_path(path: &Path, root: &Path) -> TestResult {
    if !path.is_absolute() || !path.starts_with(root) {
        return Err("path escapes case root".into());
    }
    reject_existing_symlink_ancestors(path, root)
}

pub fn sandboxed_command(
    program: &Path,
    arguments: &[String],
    environment: &[(&str, String)],
    fixture: &Path,
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> TestResult<Output> {
    require_sandbox()?;
    let canonical_fixture = fs::canonicalize(fixture)?;
    let fixture_text = canonical_fixture.to_string_lossy();
    if fixture_text.contains(['"', '\n']) {
        return Err("fixture path is not sandbox-profile safe".into());
    }
    let profile = format!(
        "(version 1) (deny default) (allow file-read*) (allow file-write* (subpath \"{fixture_text}\") (literal \"/dev/null\")) (allow process*) (allow sysctl-read) (allow mach-lookup) (deny network*)"
    );
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command.arg("-p").arg(profile).arg(program).args(arguments);
    command.env_clear();
    for (name, value) in environment {
        command.env(name, value);
    }
    command.current_dir(&canonical_fixture);
    command.process_group(0);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = command.spawn()?;
    if let Some(bytes) = stdin {
        child
            .stdin
            .take()
            .ok_or("missing subprocess stdin")?
            .write_all(bytes)?;
    }
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        if Instant::now() >= deadline {
            kill_process_group(child.id())?;
            let output = child.wait_with_output()?;
            return Err(format!(
                "sandboxed subprocess timed out after {} ms\n{}",
                timeout.as_millis(),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

pub fn assert_no_surviving_processes(fixture: &Path) -> TestResult {
    let ps = if Path::new("/bin/ps").is_file() {
        "/bin/ps"
    } else {
        "/usr/bin/ps"
    };
    let output = cleared_command(Path::new(ps), &["-axo", "command="], Path::new("/"))?;
    require_success("process survivor sentinel", &output)?;
    let needle = fixture.to_string_lossy();
    let survivors = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(needle.as_ref()))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if survivors.is_empty() {
        Ok(())
    } else {
        Err(format!("fixture subprocesses survived:\n{}", survivors.join("\n")).into())
    }
}

pub fn assert_no_forbidden_artifacts(root: &Path) -> TestResult {
    #[cfg(unix)]
    use std::os::unix::fs::FileTypeExt;
    fn walk(path: &Path) -> TestResult {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
            if metadata.is_dir()
                && name == "build"
                && path
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|parent| parent == "fixture")
            {
                continue;
            }
            #[cfg(unix)]
            if metadata.file_type().is_socket() {
                return Err(format!("socket artifact survived: {}", path.display()).into());
            }
            if metadata.is_dir() {
                walk(&path)?;
            }
            if name.ends_with("-wal")
                || name.ends_with("-shm")
                || name.ends_with("-journal")
                || name.ends_with(".db")
                || name.ends_with(".sqlite")
                || name == "checkpoint.json.tmp"
                || name == "events.jsonl.tmp"
            {
                return Err(
                    format!("forbidden oracle artifact survived: {}", path.display()).into(),
                );
            }
        }
        Ok(())
    }
    walk(root)
}

pub fn unique_root(label: &str) -> TestResult<PathBuf> {
    if label.is_empty()
        || !label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(format!("unsafe temporary-root label {label:?}").into());
    }
    let parent = [Path::new("/private/tmp"), Path::new("/tmp")]
        .into_iter()
        .find(|path| path.is_dir())
        .ok_or("no trusted system temporary directory is available")?;
    for _ in 0..64 {
        let mut random = [0_u8; 16];
        fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut nonce = String::with_capacity(random.len() * 2);
        for byte in random {
            nonce.push(HEX[(byte >> 4) as usize] as char);
            nonce.push(HEX[(byte & 0x0f) as usize] as char);
        }
        let candidate = parent.join(format!("orchestrator-rs-{label}-{nonce}"));
        match fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err("could not allocate a fresh random temporary root after 64 attempts".into())
}

pub fn require_success(context: &str, output: &Output) -> TestResult {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{context} failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into())
    }
}

fn build_environment(
    build: &Path,
    go: &Path,
    proxy: &str,
    sumdb: &str,
) -> Vec<(&'static str, String)> {
    vec![
        ("HOME", build.join("home").to_string_lossy().into_owned()),
        ("TMPDIR", build.join("tmp").to_string_lossy().into_owned()),
        ("GOTMPDIR", build.join("tmp").to_string_lossy().into_owned()),
        (
            "GOCACHE",
            build.join("gocache").to_string_lossy().into_owned(),
        ),
        (
            "GOPATH",
            build.join("gopath").to_string_lossy().into_owned(),
        ),
        (
            "GOMODCACHE",
            build.join("gopath/pkg/mod").to_string_lossy().into_owned(),
        ),
        ("GO111MODULE", "on".to_owned()),
        ("GOENV", "off".to_owned()),
        ("GOWORK", "off".to_owned()),
        ("GOSUMDB", sumdb.to_owned()),
        ("GOTOOLCHAIN", "local".to_owned()),
        ("GOTELEMETRY", "off".to_owned()),
        ("CGO_ENABLED", "0".to_owned()),
        ("TZ", "UTC".to_owned()),
        ("LANG", "C".to_owned()),
        ("LC_ALL", "C".to_owned()),
        (
            "PATH",
            go.parent()
                .unwrap_or(Path::new("/"))
                .to_string_lossy()
                .into_owned(),
        ),
        ("GOFLAGS", "-mod=readonly".to_owned()),
        ("GOPROXY", proxy.to_owned()),
    ]
}

pub fn fixed_build_environment_names() -> &'static [&'static str] {
    &FIXED_BUILD_ENVIRONMENT
}

fn verify_selected_modules(output: &[u8], manifest: &ProxyManifest) -> TestResult {
    let found = String::from_utf8(output.to_vec())?
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let expected = manifest
        .target_modules
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if found == expected {
        Ok(())
    } else {
        Err(set_mismatch("offline Go module graph", &expected, &found).into())
    }
}

fn proxy_manifest(bytes: &str) -> TestResult<ProxyManifest> {
    Ok(serde_json::from_str(bytes)?)
}

fn tree_manifest() -> TestResult<TreeManifest> {
    if sha256_bytes(TREE_MANIFEST.as_bytes())? != TREE_MANIFEST_SHA256 {
        return Err("embedded frozen-tree manifest digest mismatch".into());
    }
    Ok(serde_json::from_str(TREE_MANIFEST)?)
}

fn frozen_git_entries(roots: &[String]) -> TestResult<BTreeMap<String, (String, String, String)>> {
    let git = find_git()?;
    let (mut arguments, working_directory) = frozen_git_location()?;
    arguments.extend([
        "ls-tree".to_owned(),
        "-r".to_owned(),
        "-z".to_owned(),
        BASELINE.to_owned(),
        "--".to_owned(),
    ]);
    arguments.extend(roots.iter().cloned());
    let output = cleared_owned_command(&git, &arguments, &working_directory)?;
    require_success("reading authenticated frozen Git tree", &output)?;
    let mut entries = BTreeMap::new();
    for record in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let separator = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or("malformed git ls-tree record")?;
        let metadata = std::str::from_utf8(&record[..separator])?
            .split_whitespace()
            .collect::<Vec<_>>();
        if metadata.len() != 3 {
            return Err("malformed git ls-tree metadata".into());
        }
        let path = std::str::from_utf8(&record[separator + 1..])?.to_owned();
        if entries
            .insert(
                path,
                (
                    metadata[0].to_owned(),
                    metadata[1].to_owned(),
                    metadata[2].to_owned(),
                ),
            )
            .is_some()
        {
            return Err("duplicate path in authenticated Git tree".into());
        }
    }
    Ok(entries)
}

fn frozen_git_object(path: &str) -> TestResult<String> {
    if !safe_relative(Path::new(path)) {
        return Err(format!("unsafe frozen Git path {path:?}").into());
    }
    let git = find_git()?;
    let specification = format!("{BASELINE}:{path}");
    let (mut arguments, working_directory) = frozen_git_location()?;
    arguments.extend(["rev-parse".to_owned(), "--verify".to_owned(), specification]);
    let output = cleared_owned_command(&git, &arguments, &working_directory)?;
    require_success("reading authenticated frozen Git object", &output)?;
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn frozen_go_sums() -> TestResult<BTreeMap<(String, String), String>> {
    let git = find_git()?;
    let (mut arguments, working_directory) = frozen_git_location()?;
    arguments.extend([
        "show".to_owned(),
        format!("{BASELINE}:skills/orchestrator/go.sum"),
    ]);
    let output = cleared_owned_command(&git, &arguments, &working_directory)?;
    require_success("reading authenticated frozen go.sum", &output)?;
    let contents = String::from_utf8(output.stdout)?;
    let mut sums = BTreeMap::new();
    for line in contents.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3
            || sums
                .insert(
                    (fields[0].to_owned(), fields[1].to_owned()),
                    fields[2].to_owned(),
                )
                .is_some()
        {
            return Err("malformed or duplicate authenticated go.sum record".into());
        }
    }
    Ok(sums)
}

/// Where the frozen BASELINE commit is read from, and the leading `git`
/// arguments that name it.
///
/// This must never resolve to a silent skip: a `NANIKA_LEASE_GIT_DIR` that is
/// set but unusable is an `Err` naming exactly what was checked, and every
/// caller propagates it with `?`, so the gate goes red rather than green.
///
/// B5-DESIGN §9.H.7 precedence, first match wins:
///
/// 1. **The lease.** When `NANIKA_LEASE_GIT_DIR` is set the frozen commit is
///    read with `git --git-dir=$NANIKA_LEASE_GIT_DIR` and the walk-up below is
///    not consulted. That store holds BASELINE and nothing else: read-only,
///    unbundled from a bundle whose digest the receipt binds, with no ancestor,
///    no descendant and no commit of the tree under test in it. The walk-up
///    cannot serve the lease — inside the source snapshot four levels up from
///    the crate is the lease cache, which is not a Git work tree.
/// 2. **The checkout, bare runs only.** Unset — the four-level walk-up from
///    `CARGO_MANIFEST_DIR` and `git -C <root>`, unchanged.
fn frozen_git_location() -> TestResult<(Vec<String>, PathBuf)> {
    let Some(directory) = std::env::var_os("NANIKA_LEASE_GIT_DIR") else {
        let root = repository_root();
        return Ok((
            vec!["-C".to_owned(), root.to_string_lossy().into_owned()],
            root,
        ));
    };
    let store = PathBuf::from(directory);
    if !store.is_absolute() {
        return Err(format!(
            "NANIKA_LEASE_GIT_DIR={} is not an absolute path",
            store.display()
        )
        .into());
    }
    if !store.is_dir() {
        return Err(format!(
            "NANIKA_LEASE_GIT_DIR={} does not name a directory; the verification lease \
             did not build the frozen baseline object store",
            store.display()
        )
        .into());
    }
    for marker in ["HEAD", "objects", "refs"] {
        if !store.join(marker).exists() {
            return Err(format!(
                "NANIKA_LEASE_GIT_DIR={} is not a Git object store: {marker} is missing",
                store.display()
            )
            .into());
        }
    }
    verify_handoff_receipt(&store, "frozen-baseline-store", &["HEAD", "shallow"])?;
    Ok((
        vec![format!("--git-dir={}", store.to_string_lossy())],
        store,
    ))
}

/// The hand-off receipt version this reader accepts, and the only one.
///
/// It must stay equal to `NANIKA_HANDOFF_RECEIPT_VERSION` in
/// `scripts/cargo-cache-layout.sh`. M4g moved the shell half to
/// `nanika-handoff-v2` and left this half at v1, so every receipt the lease
/// published was refused by the only consumer that reads one from Rust — on a
/// lane no review gate runs. `nanika_require_handoff_reader_version_pin` in
/// that same file now fails when the two literals differ, and the literal here
/// is the text it greps: keep it on one line, in this shape.
pub const HANDOFF_RECEIPT_VERSION: &str = "nanika-handoff-v2";

/// The two per-run trees a hand-off may live in, as `(cache root, run
/// directory)`, or `None` when the hand-off is not inside the run its receipt
/// names.
///
/// This is the reader's copy of `lease_run_directory` in
/// `nanika_publish_handoff_receipt`/`nanika_require_handoff_receipt`, run
/// backwards. The shell is handed the configured cache root and asks whether
/// the hand-off starts with one of the two per-run prefixes under it; Rust has
/// no cache-root variable to be handed — the gatekeeper's three-variable
/// passthrough carries `NANIKA_CARGO_CACHE_ROOT` only when the operator set it,
/// and widening that allowlist is forbidden — so the same two shapes are
/// matched against the hand-off's own components and the root is *derived* from
/// the match. Deriving is strictly stronger than trusting a variable: nothing
/// outside the receipt and the hand-off's own path decides which run key is
/// read. The shallowest match wins, which is the root the shell would have
/// resolved.
///
/// Both branches require the hand-off to be *strictly inside* the run
/// directory, as `handoff.startswith(run + "/")` does on the shell side.
fn lease_run_directory(workspace: &str, nonce: &str, handoff: &Path) -> Option<(PathBuf, PathBuf)> {
    if !handoff.is_absolute() {
        return None;
    }
    let mut names = Vec::new();
    for component in handoff.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => names.push(name.to_str()?),
            // `.`, `..` and a trailing separator all make the path unequal to
            // its own normalization, which the shell publisher refuses outright.
            _ => return None,
        }
    }
    let rebuild = |count: usize| -> PathBuf {
        let mut path = PathBuf::from("/");
        for name in &names[..count] {
            path.push(name);
        }
        path
    };
    for start in 0..names.len() {
        let tail = &names[start..];
        if tail.len() > 5
            && tail[0] == "operation-receipts"
            && tail[1] == "workspaces"
            && tail[2] == workspace
            && tail[3] == "runs"
            && tail[4] == nonce
        {
            return Some((rebuild(start), rebuild(start + 5)));
        }
        if tail.len() > 6
            && tail[0] == "cargo-target"
            && tail[1] == "workspaces"
            && tail[2] == workspace
            && !tail[3].is_empty()
            && tail[4] == "runs"
            && tail[5] == nonce
        {
            return Some((rebuild(start), rebuild(start + 6)));
        }
    }
    None
}

/// Opens one path without following a final symlink, the way every reader on
/// the shell side does with `os.O_RDONLY | os.O_NOFOLLOW`.
fn open_no_follow(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
}

/// Reads one owned single-link regular file, refusing anything larger than
/// `limit`. The checks run against the *opened descriptor*, so the file that is
/// hashed is the file that was checked.
fn read_owned_file(path: &Path, limit: u64) -> TestResult<Vec<u8>> {
    let mut file = open_no_follow(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(format!("{} is not a single-link regular file", path.display()).into());
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(format!("{} is not owned by this user", path.display()).into());
    }
    if metadata.len() > limit {
        return Err(format!("{} is oversized", path.display()).into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)?;
    if bytes.len() as u64 != metadata.len() {
        return Err(format!("{} changed while it was read", path.display()).into());
    }
    Ok(bytes)
}

/// The per-run key, read from the run the receipt names, with the shell
/// reader's four conditions on it: a regular file, one link, this uid, and no
/// group or other permission bits. The bytes are never logged; only the
/// verdict leaves this function.
fn lease_run_key(run: &Path) -> TestResult<Vec<u8>> {
    let path = run.join("run.key");
    let mut file = open_no_follow(&path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err("the lease run key is not a private owned regular file".into());
    }
    let mut material = Vec::new();
    Read::by_ref(&mut file)
        .take(65)
        .read_to_end(&mut material)?;
    if material.len() != 64 || !material.iter().copied().all(is_lowercase_hex_byte) {
        return Err("the lease run key is malformed".into());
    }
    Ok(material)
}

/// HMAC-SHA256, RFC 2104, over the exact byte construction
/// `nanika_publish_handoff_receipt`'s `receipt_mac` uses: the receipt's own
/// preceding lines joined with `\n` and terminated with one `\n`, keyed by the
/// run key's 64 ASCII bytes as read from disk. There is no `hmac` crate in the
/// locked dependency graph and adding one would move `Cargo.lock`, so the two
/// SHA-256 passes are written out here.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    const BLOCK: usize = 64;
    let mut block = [0_u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36_u8; BLOCK];
    let mut outer_pad = [0x5c_u8; BLOCK];
    for index in 0..BLOCK {
        inner_pad[index] ^= block[index];
        outer_pad[index] ^= block[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner);
    outer.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    text
}

/// Constant-time equality over two same-length ASCII digests.
fn digests_match(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (a, b) in left.bytes().zip(right.bytes()) {
        difference |= a ^ b;
    }
    difference == 0
}

/// The shell side spells every digest, nonce and key `[0-9a-f]`; uppercase is
/// refused there, so it is refused here.
fn is_lowercase_hex_byte(byte: u8) -> bool {
    byte.is_ascii_digit() || byte.is_ascii_lowercase() && byte <= b'f'
}

fn is_lowercase_hex(value: &str, width: usize) -> bool {
    value.len() == width && value.bytes().all(is_lowercase_hex_byte)
}

/// M4f/RW1, corrected by M4g and by blocker Y. `NANIKA_LEASE_GIT_DIR` is a
/// lease hand-off, and until the receipt existed the harness believed any
/// directory that merely looked like a bare repository. The lease publishes
/// `<hand-off>.sha256` beside every hand-off it produces
/// (`nanika_publish_handoff_receipt`); this is the reader's half.
///
/// It mirrors `nanika_require_handoff_receipt` check for check, in the same
/// order: the receipt is an owned single-link regular file of bounded size and
/// ASCII with a final newline; its first line is exactly
/// [`HANDOFF_RECEIPT_VERSION`]; its five header fields are present and
/// well-formed; the hand-off lies inside the run directory the `workspace-id`
/// and `lease-nonce` name; the run's 0600 `run.key` is readable and
/// well-formed; the trailing `mac` line verifies as HMAC-SHA256 over the
/// preceding lines under that key; and every `entry` line's digest still
/// matches its payload, with the payloads this caller depends on among them —
/// `shallow` is the pin itself, so a store answering for a different commit
/// cannot pass.
///
/// Two checks stay the shell reader's alone and are not weakened here, only
/// unowned. `lease-id` is compared to the host's verification-lock inode by
/// `nanika_require_handoff_receipt`, which resolves the account home through
/// `nanika_fixed_lease_root`; this reader has no such resolver and requires the
/// field to be present and non-empty, as the v1 reader did. Whether the named
/// run is still *live* is likewise the shell reader's call. A missing or
/// mismatched receipt is an `Err` here, never a skip.
fn verify_handoff_receipt(handoff: &Path, kind: &str, required: &[&str]) -> TestResult {
    let receipt_path = PathBuf::from(format!("{}.sha256", handoff.to_string_lossy()));
    let refuse = |message: String| -> Box<dyn std::error::Error> {
        format!("{}: {message}", receipt_path.display()).into()
    };
    let raw = read_owned_file(&receipt_path, 64 * 1024)
        .map_err(|error| refuse(format!("is unreadable: {error}")))?;
    if !raw.is_ascii() {
        return Err(refuse("is not ASCII".to_owned()));
    }
    let text = String::from_utf8(raw).map_err(|_| refuse("is not ASCII".to_owned()))?;
    let body = text
        .strip_suffix('\n')
        .ok_or_else(|| refuse("is truncated".to_owned()))?;
    let lines: Vec<&str> = body.split('\n').collect();
    if lines.len() < 8 || lines[0] != HANDOFF_RECEIPT_VERSION {
        return Err(refuse(format!("is not a {HANDOFF_RECEIPT_VERSION} record")));
    }
    let mut header = BTreeMap::new();
    for (index, name) in [
        "kind",
        "lease-id",
        "lease-nonce",
        "lease-operation",
        "workspace-id",
    ]
    .into_iter()
    .enumerate()
    {
        let value = lines[index + 1]
            .strip_prefix(&format!("{name} "))
            .ok_or_else(|| refuse(format!("has no {name} line")))?;
        header.insert(name, value.to_owned());
    }
    if header["kind"] != kind {
        return Err(refuse(format!(
            "records another kind of hand-off, not {kind}"
        )));
    }
    if header["lease-id"].is_empty() {
        return Err(refuse("records no usable lease-id".to_owned()));
    }
    let nonce = header["lease-nonce"].clone();
    let workspace = header["workspace-id"].clone();
    if !is_lowercase_hex(&nonce, 32) {
        return Err(refuse("records no lease nonce".to_owned()));
    }
    if header["lease-operation"].is_empty()
        || !header["lease-operation"]
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(refuse("records no lease operation".to_owned()));
    }
    if !is_lowercase_hex(&workspace, 64) {
        return Err(refuse("records no workspace identity".to_owned()));
    }
    let Some((_, run)) = lease_run_directory(&workspace, &nonce, handoff) else {
        return Err(refuse(
            "the hand-off is not inside the lease run its receipt names".to_owned(),
        ));
    };
    let key = lease_run_key(&run)
        .map_err(|error| refuse(format!("the lease run key is unusable: {error}")))?;
    let authenticated = lines[lines.len() - 1]
        .strip_prefix("mac ")
        .filter(|code| is_lowercase_hex(code, 64))
        .ok_or_else(|| refuse("carries no lease authentication code".to_owned()))?;
    let message = format!("{}\n", lines[..lines.len() - 1].join("\n"));
    if !digests_match(&hex(&hmac_sha256(&key, message.as_bytes())), authenticated) {
        return Err(refuse(
            "the lease authentication code does not verify".to_owned(),
        ));
    }
    let mut recorded = BTreeMap::new();
    for line in &lines[6..lines.len() - 1] {
        let entry = line
            .strip_prefix("entry ")
            .ok_or_else(|| refuse("carries an unrecognized line".to_owned()))?;
        let (digest, name) = entry
            .split_once(' ')
            .ok_or_else(|| refuse("carries a malformed entry".to_owned()))?;
        if !is_lowercase_hex(digest, 64) {
            return Err(refuse("carries a malformed entry digest".to_owned()));
        }
        if recorded
            .insert(name.to_owned(), digest.to_owned())
            .is_some()
        {
            return Err(refuse(format!("names the payload {name} twice")));
        }
    }
    for name in required {
        if !recorded.contains_key(*name) {
            return Err(refuse(format!("does not account for the payload {name}")));
        }
    }
    if recorded.is_empty() {
        return Err(refuse("accounts for no payload at all".to_owned()));
    }
    for (name, digest) in &recorded {
        let payload = handoff_payload_path(handoff, name)
            .ok_or_else(|| refuse(format!("names an invalid payload {name}")))?;
        let bytes = read_owned_file(&payload, u64::MAX)
            .map_err(|error| refuse(format!("names an unreadable payload {name}: {error}")))?;
        let observed = {
            use sha2::Digest as _;
            hex(&sha2::Sha256::digest(&bytes))
        };
        if !digests_match(&observed, digest) {
            return Err(refuse(format!(
                "payload {name} does not match the digest the lease published"
            )));
        }
    }
    Ok(())
}

/// The shell publisher's `payload_path`: `.` names the hand-off itself, and
/// every other name is relative, non-empty, without a leading or trailing
/// separator and without an empty, `.` or `..` component.
fn handoff_payload_path(handoff: &Path, name: &str) -> Option<PathBuf> {
    if name == "." {
        return Some(handoff.to_path_buf());
    }
    if name.is_empty() || name.starts_with('/') || name.ends_with('/') {
        return None;
    }
    if name
        .split('/')
        .any(|component| matches!(component, "" | "." | ".."))
    {
        return None;
    }
    Some(handoff.join(name))
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.."))
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn find_git() -> TestResult<PathBuf> {
    find_allowlisted(&["/usr/bin/git", "/opt/homebrew/bin/git"])
}

fn find_go() -> TestResult<PathBuf> {
    find_allowlisted(&[
        "/opt/homebrew/bin/go",
        "/usr/local/go/bin/go",
        "/usr/bin/go",
    ])
}

fn find_allowlisted(candidates: &[&str]) -> TestResult<PathBuf> {
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .ok_or_else(|| format!("none of the allowlisted executables exists: {candidates:?}").into())
}

fn require_sandbox() -> TestResult {
    if cfg!(target_os = "macos") && Path::new("/usr/bin/sandbox-exec").is_file() {
        Ok(())
    } else {
        Err("deny-network oracle sandbox is unavailable; refusing to run".into())
    }
}

fn require_go_version(value: &str) -> TestResult {
    let version = value
        .split_whitespace()
        .filter_map(|part| part.strip_prefix("go"))
        .find(|part| part.as_bytes().first().is_some_and(u8::is_ascii_digit))
        .ok_or("Go version output omitted a goX.Y.Z token")?;
    let numbers = version
        .split('.')
        .take(3)
        .map(|part| {
            part.trim_end_matches(|character: char| !character.is_ascii_digit())
                .parse::<u64>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    if numbers.len() != 3 || (numbers[0], numbers[1], numbers[2]) < REQUIRED_GO {
        return Err(format!("Go compiler {version} is older than 1.25.4").into());
    }
    Ok(())
}

fn cleared_command(program: &Path, arguments: &[&str], directory: &Path) -> TestResult<Output> {
    let mut command = Command::new(program);
    command.args(arguments).current_dir(directory).env_clear();
    Ok(command.output()?)
}

fn cleared_owned_command(
    program: &Path,
    arguments: &[String],
    directory: &Path,
) -> TestResult<Output> {
    let mut command = Command::new(program);
    command.args(arguments).current_dir(directory).env_clear();
    Ok(command.output()?)
}

fn regular_files(root: &Path) -> TestResult<BTreeSet<String>> {
    fn walk(root: &Path, path: &Path, files: &mut BTreeSet<String>) -> TestResult {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "symlink is forbidden in authenticated tree: {}",
                    path.display()
                )
                .into());
            }
            if metadata.is_dir() {
                walk(root, &path, files)?;
            } else if metadata.is_file() {
                files.insert(
                    path.strip_prefix(root)?
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            } else {
                return Err(
                    format!("non-regular authenticated tree entry: {}", path.display()).into(),
                );
            }
        }
        Ok(())
    }
    let mut files = BTreeSet::new();
    walk(root, root, &mut files)?;
    Ok(files)
}

fn sha256_files(paths: &[PathBuf]) -> TestResult<Vec<String>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut command = Command::new("/usr/bin/shasum");
    command.args(["-a", "256"]).args(paths).env_clear();
    let output = command.output()?;
    require_success("hashing authenticated files", &output)?;
    let hashes = String::from_utf8(output.stdout)?
        .lines()
        .map(|line| {
            line.split_whitespace()
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect::<Vec<_>>();
    if hashes.len() != paths.len() {
        return Err("SHA-256 tool returned an unexpected result count".into());
    }
    Ok(hashes)
}

pub fn sha256_bytes(bytes: &[u8]) -> TestResult<String> {
    let mut child = Command::new("/usr/bin/shasum")
        .args(["-a", "256"])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("SHA-256 tool has no stdin")?
        .write_all(bytes)?;
    let output = child.wait_with_output()?;
    require_success("hashing authenticated bytes", &output)?;
    Ok(String::from_utf8(output.stdout)?
        .split_whitespace()
        .next()
        .ok_or("SHA-256 tool returned no digest")?
        .to_owned())
}

fn make_read_only(root: &Path) -> TestResult {
    fn walk(path: &Path) -> TestResult {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                walk(&path)?;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o555))?;
            } else if metadata.is_file() {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o444))?;
            } else {
                return Err(format!("non-regular module-cache entry: {}", path.display()).into());
            }
        }
        Ok(())
    }
    walk(root)?;
    fs::set_permissions(root, fs::Permissions::from_mode(0o555))?;
    Ok(())
}

pub fn make_writable(root: &Path) -> TestResult {
    fn walk(path: &Path) -> TestResult {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
            for entry in fs::read_dir(path)? {
                walk(&entry?.path())?;
            }
        } else if metadata.is_file() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o644))?;
        }
        Ok(())
    }
    if root.exists() {
        walk(root)?;
    }
    Ok(())
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn is_path_environment(name: &str) -> bool {
    matches!(
        name,
        "HOME"
            | "ORCHESTRATOR_CONFIG_DIR"
            | "ALLUKA_HOME"
            | "VIA_HOME"
            | "ORCHESTRATOR_PERSONAS_DIR"
            | "TMPDIR"
            | "PATH"
    )
}

fn reject_existing_symlink_ancestors(path: &Path, root: &Path) -> TestResult {
    let mut current = root.to_path_buf();
    for component in path.strip_prefix(root)?.components() {
        current.push(component.as_os_str());
        if current.exists() && fs::symlink_metadata(&current)?.file_type().is_symlink() {
            return Err(
                format!("environment path traverses symlink: {}", current.display()).into(),
            );
        }
    }
    Ok(())
}

fn kill_process_group(pid: u32) -> TestResult {
    let group = format!("-{pid}");
    let output = Command::new("/bin/kill")
        .args(["-KILL", &group])
        .env_clear()
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "failed to kill subprocess group {group}: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into())
    }
}

fn set_mismatch(label: &str, expected: &BTreeSet<String>, actual: &BTreeSet<String>) -> String {
    let missing = expected
        .difference(actual)
        .take(20)
        .cloned()
        .collect::<Vec<_>>();
    let extra = actual
        .difference(expected)
        .take(20)
        .cloned()
        .collect::<Vec<_>>();
    format!("{label} mismatch\nmissing: {missing:#?}\nextra: {extra:#?}")
}

// ── Blocker Y. The hand-off reader's own gates. ─────────────────────────────
//
// These five cases run with no verification lease. `nanika_lease_lock_identity`
// recomputes the lock inode rather than reading it out of the environment —
// that is exactly why `nanika_require_handoff_receipt` can check `lease-id`
// from a consumer that holds nothing — so the real shell publisher can be
// driven against a scratch cache root here, and the receipt under test is the
// one the lease's own code writes rather than a hand-authored fixture. That is
// the half of the drift guard a static pin cannot give: a pin proves the two
// version literals agree, and this proves the reader accepts what the publisher
// actually emits.

/// The kind and payload names the frozen-baseline store hand-off uses, so the
/// cases exercise the same arguments `frozen_git_location` passes.
const PROBE_KIND: &str = "frozen-baseline-store";
const PROBE_PAYLOADS: [&str; 2] = ["HEAD", "shallow"];

/// The publisher driver, verbatim shell. It sources the reviewed layout library
/// and calls `nanika_publish_handoff_receipt`; nothing here reimplements the
/// receipt format.
const PUBLISH_PROBE_DRIVER: &str = r#"#!/bin/sh
set -eu
layout=$1
cache_root=$2
workspace=$3
nonce=$4
. "$layout"
NANIKA_CARGO_CACHE_ROOT=$cache_root
NANIKA_CARGO_LEASE_WORKSPACE_ID=$workspace
NANIKA_CARGO_LEASE_NONCE=$nonce
NANIKA_CARGO_LEASE_OPERATION=evidence
NANIKA_CARGO_LEASE_ID=$(nanika_lease_lock_identity)
export NANIKA_CARGO_CACHE_ROOT NANIKA_CARGO_LEASE_WORKSPACE_ID \
  NANIKA_CARGO_LEASE_NONCE NANIKA_CARGO_LEASE_OPERATION NANIKA_CARGO_LEASE_ID
nanika_prepare_cargo_cache_root >/dev/null
run_directory=$(nanika_prepare_private_tree "$cache_root" \
  "operation-receipts/workspaces/$workspace/runs/$nonce" 'reader-probe lease run')
nanika_write_lease_run_key "$run_directory"
nanika_require_owned_file "$run_directory/run.key" 'reader-probe run key'
handoff=$(nanika_prepare_private_tree "$cache_root" \
  "operation-receipts/workspaces/$workspace/runs/$nonce/handoff" 'reader-probe hand-off')
printf 'ref: refs/heads/frozen-baseline\n' >"$handoff/HEAD"
printf '3f2e5d1b9acfbe4a4338bb003425668cc803464e\n' >"$handoff/shallow"
$NANIKA_TOOL_CHMOD 0600 "$handoff/HEAD" "$handoff/shallow"
nanika_publish_handoff_receipt "$handoff" frozen-baseline-store HEAD shallow
nanika_require_handoff_receipt "$handoff" 'reader probe' frozen-baseline-store \
  HEAD shallow
"#;

/// One scratch cache root holding one live lease run and one published hand-off.
struct PublishedHandoff {
    outer: PathBuf,
    cache_root: PathBuf,
    handoff: PathBuf,
}

impl Drop for PublishedHandoff {
    fn drop(&mut self) {
        let _ = make_writable(&self.outer);
        let _ = fs::remove_dir_all(&self.outer);
    }
}

impl PublishedHandoff {
    fn publish() -> TestResult<Self> {
        let outer = unique_root("handoff")?;
        // Built before the driver runs so the `Drop` below removes the scratch
        // root on every failure path, not only on the happy one.
        let mut published = Self {
            cache_root: outer.join("cache"),
            handoff: PathBuf::new(),
            outer,
        };
        let driver = published.outer.join("publish-handoff-probe.sh");
        fs::write(&driver, PUBLISH_PROBE_DRIVER)?;
        fs::set_permissions(&driver, fs::Permissions::from_mode(0o600))?;
        let layout = workspace_root().join("scripts/cargo-cache-layout.sh");
        if !layout.is_file() {
            return Err(format!("{} is missing", layout.display()).into());
        }
        let workspace = random_lowercase_hex(32)?;
        let nonce = random_lowercase_hex(16)?;
        let output = Command::new("/bin/sh")
            .arg(&driver)
            .arg(&layout)
            .arg(&published.cache_root)
            .arg(&workspace)
            .arg(&nonce)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .output()?;
        require_success("publishing a probe hand-off receipt", &output)?;
        published.handoff = published
            .cache_root
            .join("operation-receipts/workspaces")
            .join(&workspace)
            .join("runs")
            .join(&nonce)
            .join("handoff");
        Ok(published)
    }

    fn receipt(&self) -> PathBuf {
        PathBuf::from(format!("{}.sha256", self.handoff.to_string_lossy()))
    }

    fn verify(&self) -> TestResult {
        verify_handoff_receipt(&self.handoff, PROBE_KIND, &PROBE_PAYLOADS)
    }

    /// Rewrites the receipt in place, preserving its owner-only mode and its
    /// single link, so only the bytes under test change.
    fn rewrite_receipt(&self, rewrite: impl FnOnce(String) -> String) -> TestResult {
        let path = self.receipt();
        let text = fs::read_to_string(&path)?;
        fs::write(&path, rewrite(text))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }
}

fn random_lowercase_hex(bytes: usize) -> TestResult<String> {
    let mut material = vec![0_u8; bytes];
    fs::File::open("/dev/urandom")?.read_exact(&mut material)?;
    Ok(hex(&material))
}

fn expect_refusal(result: TestResult, expected: &str) -> TestResult {
    match result {
        Ok(()) => Err(format!("the hand-off reader accepted a receipt it must refuse; it should have said {expected:?}").into()),
        Err(error) => {
            let message = error.to_string();
            if message.contains(expected) {
                Ok(())
            } else {
                Err(format!("the hand-off reader refused for the wrong reason: {message:?} does not carry {expected:?}").into())
            }
        }
    }
}

#[test]
fn handoff_receipt_reader_accepts_the_published_v2_receipt() -> TestResult {
    let published = PublishedHandoff::publish()?;
    let text = fs::read_to_string(published.receipt())?;
    let first = text.lines().next().unwrap_or_default();
    if first != HANDOFF_RECEIPT_VERSION {
        return Err(format!(
            "the shell publisher wrote a {first:?} receipt, not {HANDOFF_RECEIPT_VERSION:?}"
        )
        .into());
    }
    published.verify()
}

#[test]
fn handoff_receipt_reader_refuses_a_flipped_authentication_code() -> TestResult {
    let published = PublishedHandoff::publish()?;
    published.verify()?;
    published.rewrite_receipt(|text| {
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        let last = lines.len() - 1;
        let code = lines[last].trim_start_matches("mac ").to_owned();
        // One nibble, not one line: every other field, the layout and the run
        // key stay exactly as the publisher left them.
        let flipped = if code.starts_with('0') { '1' } else { '0' };
        lines[last] = format!("mac {flipped}{}", &code[1..]);
        format!("{}\n", lines.join("\n"))
    })?;
    expect_refusal(
        published.verify(),
        "the lease authentication code does not verify",
    )
}

#[test]
fn handoff_receipt_reader_refuses_a_hand_off_outside_its_lease_run() -> TestResult {
    let published = PublishedHandoff::publish()?;
    published.verify()?;
    // The M4f forgery's shape, which the v1 receipt accepted: same cache root,
    // same owner-only chain, the lease's own receipt bytes verbatim — and a
    // hand-off that is not inside the run the receipt names.
    let moved = published.cache_root.join("handoff-copy");
    fs::DirBuilder::new().mode(0o700).create(&moved)?;
    for name in PROBE_PAYLOADS {
        fs::copy(published.handoff.join(name), moved.join(name))?;
    }
    fs::copy(
        published.receipt(),
        format!("{}.sha256", moved.to_string_lossy()),
    )?;
    expect_refusal(
        verify_handoff_receipt(&moved, PROBE_KIND, &PROBE_PAYLOADS),
        "the hand-off is not inside the lease run its receipt names",
    )
}

#[test]
fn handoff_receipt_reader_refuses_a_stale_v1_record() -> TestResult {
    let published = PublishedHandoff::publish()?;
    published.verify()?;
    // Blocker Y in one line: the version the reader accepted before this fix.
    published
        .rewrite_receipt(|text| text.replacen(HANDOFF_RECEIPT_VERSION, "nanika-handoff-v1", 1))?;
    expect_refusal(
        published.verify(),
        &format!("is not a {HANDOFF_RECEIPT_VERSION} record"),
    )
}

#[test]
fn handoff_receipt_reader_refuses_a_payload_changed_after_publication() -> TestResult {
    let published = PublishedHandoff::publish()?;
    published.verify()?;
    let payload = published.handoff.join("shallow");
    let mut bytes = fs::read(&payload)?;
    bytes[0] ^= 0x01;
    fs::write(&payload, bytes)?;
    fs::set_permissions(&payload, fs::Permissions::from_mode(0o600))?;
    expect_refusal(
        published.verify(),
        "payload shallow does not match the digest the lease published",
    )
}
